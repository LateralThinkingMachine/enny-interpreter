use meeting_call_pilot::{
    auth::Role, config::Config, engine::ProviderActivity, http::hosted_router,
    session_host::SessionHost,
};
use std::{future::IntoFuture, io::Read, path::Path, process::ExitCode, time::Duration};
use tokio::sync::oneshot;

// The desktop app sends one length-prefixed JSON configuration over an anonymous
// pipe. Keeping the writer open ties the paid process to the launcher's lifetime.
fn desktop_config() -> Result<(Config, oneshot::Receiver<()>), &'static str> {
    use std::os::fd::AsFd;
    if !std::fs::File::from(
        std::io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|_| "private_pipe_required")?,
    )
    .metadata()
    .is_ok_and(|metadata| {
        use std::os::unix::fs::FileTypeExt;
        metadata.file_type().is_fifo()
    }) {
        return Err("private_pipe_required");
    }
    let mut input = std::io::stdin().lock();
    let mut header = [0; 4];
    input
        .read_exact(&mut header)
        .map_err(|_| "invalid_config_frame")?;
    let length = u32::from_be_bytes(header) as usize;
    if !(1..=16 * 1024).contains(&length) {
        return Err("invalid_config_frame");
    }
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .map_err(|_| "invalid_config_frame")?;
    let config = Config::decode_desktop(&bytes);
    bytes.fill(0);
    let config = config?;
    drop(input);
    let (closed, exit) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = std::io::stdin().read_exact(&mut [0]);
        let _ = closed.send(());
    });
    Ok((config, exit))
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(status) => status,
        Err(code) => {
            eprintln!("call-pilot: {code}");
            ExitCode::FAILURE
        }
    }
}

// The paid Mac and Linux launchers consume only these three normal exits.
// Every other status, signal, or missing result leaves provider closure unknown.
fn desktop_exit(activity: ProviderActivity) -> ExitCode {
    ExitCode::from(match activity {
        ProviderActivity::NoneAdmitted => 20,
        ProviderActivity::CloseObserved => 21,
        ProviderActivity::Unconfirmed => 22,
    })
}

async fn run() -> Result<ExitCode, &'static str> {
    let args = std::env::args().collect::<Vec<_>>();
    let (mut config, parent) = match &args[1..] {
        [flag, path] if flag == "--config" => (Config::load(Path::new(path))?, None),
        [flag] if flag == "--desktop-pipe" => {
            let (config, parent) = desktop_config()?;
            (config, Some(parent))
        }
        _ => return Err("expected_private_config"),
    };
    let paid_desktop = config.paid_desktop_meeting();
    let origins = config.origins()?;
    let (host, ticket) = SessionHost::new(
        config.provider.take(),
        config.duration_seconds,
        config.max_input_audio_seconds,
        origins,
        config.assets_dir.clone(),
        config.repeatable_sessions,
    )?;
    let pilot = host.pilot();
    let listener = tokio::net::TcpListener::bind(origins.host(Role::Operator))
        .await
        .map_err(|_| "loopback_bind_unavailable")?;
    let public_listener = tokio::net::TcpListener::bind(origins.host(Role::Public))
        .await
        .map_err(|_| "public_loopback_bind_unavailable")?;
    config.write_credentials(&ticket)?;
    let app = hosted_router(host.clone(), Role::Operator);
    let public_app = hosted_router(host.clone(), Role::Public);
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(None);
    let mut graceful_rx = shutdown_rx.clone();
    let mut public_graceful_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        let mut termination =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        let deadline = async {
            match pilot.deadline() {
                Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(deadline);
        let reason = tokio::select! {
            _=tokio::signal::ctrl_c()=>"process_shutdown",
            _=async { match parent { Some(closed) => { let _ = closed.await; }, None => std::future::pending::<()>().await } }=>"process_shutdown",
            _=&mut deadline=>"deadline",
            _=async { if let Some(signal)=termination.as_mut(){signal.recv().await;}else{std::future::pending::<()>().await;} }=>"process_shutdown",
        };
        pilot.stop(reason);
        // Let any in-flight creation retire before publishing a result. The HTTP
        // listeners then have three more seconds to complete local shutdown.
        let result =
            match tokio::time::timeout(Duration::from_secs(32), host.shutdown_with_receipt()).await
            {
                Ok(Some(activity)) if paid_desktop => Ok(desktop_exit(activity)),
                Ok(Some(_)) => Ok(ExitCode::SUCCESS),
                _ => Err("provider_shutdown_uncertain"),
            };
        shutdown_tx.send_replace(Some(result));
    });
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = graceful_rx.changed().await;
        })
        .into_future();
    let public_server = axum::serve(public_listener, public_app)
        .with_graceful_shutdown(async move {
            let _ = public_graceful_rx.changed().await;
        })
        .into_future();
    let server = futures_util::future::try_join(server, public_server);
    tokio::pin!(server);
    tokio::select! {
        result=&mut server=>result.map(|_|()).map_err(|_|"server_unavailable"),
        _=shutdown_rx.changed()=>tokio::time::timeout(Duration::from_secs(3),&mut server).await.map_err(|_|"shutdown_deadline")?.map(|_|()).map_err(|_|"server_unavailable"),
    }?;
    shutdown_rx.borrow().unwrap_or(Err("shutdown_unobserved"))
}
