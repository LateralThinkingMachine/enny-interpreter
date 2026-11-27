#import <Cocoa/Cocoa.h>
#import <arpa/inet.h>
#import <fcntl.h>
#import <signal.h>
#import <sys/stat.h>
#import <unistd.h>

static NSString *const origin = @"http://127.0.0.1:5180";

// Foundation can abbreviate /private/var to /var again after resolving it.
// Use the physical POSIX path that the service validates component by component.
static NSString *physicalPath(NSString *path) {
    char *resolved = realpath(path.fileSystemRepresentation, NULL);
    if (!resolved) return nil;
    NSString *result = [NSFileManager.defaultManager stringWithFileSystemRepresentation:resolved length:strlen(resolved)];
    free(resolved);
    return result;
}

// The approval uses whole US cents. Round the two-stream quote up to a cent
// so a limit shown as sufficient can admit both streams for the full duration.
static NSInteger minimumCents(NSInteger minutes) {
    return (minutes * 67 + 9) / 10;
}

static NSString *dollars(NSInteger cents) {
    return [NSString stringWithFormat:@"US$%ld.%02ld", (long)(cents / 100), (long)(cents % 100)];
}

static NSString *estimateText(NSInteger minutes) {
    return [NSString stringWithFormat:@"Estimated OpenAI cost for one listener is %@ for %ld %@ of captions and interpretation.",
            dollars(minimumCents(minutes)), (long)minutes, minutes == 1 ? @"minute" : @"minutes"];
}

static NSString *approvalStatus(NSInteger minutes, NSString *problem) {
    if (minutes <= 0) return problem;
    NSString *quote = estimateText(minutes);
    return problem ? [NSString stringWithFormat:@"%@ %@", quote, problem] : quote;
}

static BOOL hasFormat(NSString *text, NSString *format) {
    return text && [text rangeOfString:format options:NSRegularExpressionSearch].location != NSNotFound;
}

static NSString *approvalError(NSString *duration, NSString *budget, NSInteger *minutes, NSInteger *cents) {
    if (!hasFormat(duration, @"\\A[1-9][0-9]?\\z") || duration.integerValue > 60)
        return @"Enter a whole number of minutes from 1 to 60.";
    *minutes = duration.integerValue;
    if (!hasFormat(budget, @"\\A(?:0|[1-9][0-9]{0,3})(?:\\.[0-9]{1,2})?\\z"))
        return @"Enter a cost limit in US dollars, such as 5.00.";
    NSArray<NSString *> *parts = [budget componentsSeparatedByString:@"."];
    NSString *fraction = parts.count == 2 ? parts[1] : @"";
    *cents = parts[0].integerValue * 100 + fraction.integerValue * (fraction.length == 1 ? 10 : 1);
    if (*cents < 1 || *cents > 100000)
        return @"Enter a cost limit from US$0.01 to US$1000.00.";
    if (*cents < minimumCents(*minutes))
        return [NSString stringWithFormat:@"Choose a local limit of %@ or more.", dollars(minimumCents(*minutes))];
    return nil;
}

// Only these two paid --desktop-pipe statuses carry favorable receipts.
enum { NoProviderWork = 20, AllConnectionsClosed = 21, ClosureUnconfirmed = 22 };

static NSString *paidShutdownMessage(int receipt) {
    if (receipt == NoProviderWork) return @"Local audio stopped. Enny did not start any OpenAI work.";
    if (receipt == AllConnectionsClosed) return @"Local audio stopped. OpenAI confirmed that all admitted connections closed.";
    return @"Local audio stopped. OpenAI closure is unconfirmed. Check usage in your OpenAI account.";
}

@interface Session : NSObject
@property (strong) NSTask *task;
@property BOOL checking;
@property BOOL launched;
@property (strong) NSFileHandle *writer;
@property (copy) NSString *directory;
@property (nonatomic, copy) NSURL *entry;
- (BOOL)startWithKey:(NSString *)key minutes:(NSInteger)minutes budgetCents:(NSInteger)cents;
- (void)stop;
- (int)paidShutdownReceipt;
@end

@implementation Session
- (BOOL)startWithKey:(NSString *)key minutes:(NSInteger)minutes budgetCents:(NSInteger)cents {
    NSURL *resources = NSBundle.mainBundle.resourceURL;
    NSString *assets = physicalPath([resources URLByAppendingPathComponent:@"web"].path);
    NSString *temp = physicalPath(NSTemporaryDirectory());
    if (!assets || !temp) return NO;
    NSString *template = [temp stringByAppendingPathComponent:@"interpreter.XXXXXXXX"];
    char *name = strdup(template.fileSystemRepresentation);
    if (!name) return NO;
    if (mkdtemp(name)) self.directory = [NSFileManager.defaultManager
        stringWithFileSystemRepresentation:name length:strlen(name)];
    free(name);
    if (!self.directory) return NO;

    NSMutableDictionary *config = [@{@"assets_dir": assets,
        @"credentials_path": [self.directory stringByAppendingPathComponent:@"credentials.json"],
        @"operator_port": @5180} mutableCopy];
    if (key) {
        config[@"meeting_minutes"] = @(minutes);
        config[@"budget_cents"] = @(cents);
        config[@"api_key"] = key;
    } else {
        config[@"duration_seconds"] = @600;
        config[@"max_input_audio_seconds"] = @1200;
        config[@"provider"] = NSNull.null;
    }
    NSData *data = [NSJSONSerialization dataWithJSONObject:config options:0 error:nil];
    if (!data || data.length > 16384) return NO;

    NSPipe *pipe = [NSPipe pipe];
    self.writer = pipe.fileHandleForWriting;
    self.task = [[NSTask alloc] init];
    self.task.executableURL = [NSBundle.mainBundle.bundleURL URLByAppendingPathComponent:
                               @"Contents/Helpers/meeting-call-pilot"];
    self.task.arguments = @[@"--desktop-pipe"];
    self.task.environment = @{@"PATH": @"/usr/bin:/bin", @"LANG": @"C"};
    self.task.currentDirectoryURL = [NSURL fileURLWithPath:self.directory isDirectory:YES];
    self.task.standardInput = pipe;
    self.task.standardOutput = NSFileHandle.fileHandleWithNullDevice;
    self.task.standardError = self.checking ? NSFileHandle.fileHandleWithStandardError : NSFileHandle.fileHandleWithNullDevice;
    if (![self.task launchAndReturnError:nil]) return NO;
    self.launched = YES;
    uint32_t length = htonl((uint32_t)data.length);
    return [self.writer writeData:[NSData dataWithBytes:&length length:sizeof(length)] error:nil]
        && [self.writer writeData:data error:nil];
}

// The backend creates this file only after owning both loopback listeners.
// Consume it once and pass the fragment directly to Chrome via LaunchServices.
- (NSURL *)entry {
    if (_entry) return _entry;
    if (!self.task.running) return nil;
    NSString *path = [self.directory stringByAppendingPathComponent:@"credentials.json"];
    int fd = open(path.fileSystemRepresentation, O_RDONLY | O_NOFOLLOW | O_NONBLOCK);
    if (fd == -1) return nil;
    struct stat info;
    char buffer[1024];
    ssize_t size = -1;
    if (fstat(fd, &info) == 0 && S_ISREG(info.st_mode) && (info.st_mode & 0777) == 0600
        && info.st_uid == geteuid() && info.st_nlink == 1 && info.st_size > 0 && info.st_size < sizeof(buffer))
        size = read(fd, buffer, sizeof(buffer));
    close(fd);
    if (size <= 0 || size != info.st_size) return nil;
    id value = [NSJSONSerialization JSONObjectWithData:[NSData dataWithBytes:buffer length:size] options:0 error:nil];
    if (![value isKindOfClass:NSDictionary.class] || ![value[@"origin"] isEqual:origin]) return nil;
    NSString *ticket = value[@"bootstrap"];
    NSCharacterSet *invalid = [[NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdef"] invertedSet];
    if (![ticket isKindOfClass:NSString.class] || ticket.length != 64 ||
        [ticket rangeOfCharacterFromSet:invalid].location != NSNotFound) return nil;
    if (unlink(path.fileSystemRepresentation) != 0 || !self.task.running) return nil;
    _entry = [NSURL URLWithString:[NSString stringWithFormat:@"%@/#bootstrap=%@", origin, ticket]];
    return _entry;
}

- (int)paidShutdownReceipt {
    if (!self.launched) return NoProviderWork;
    if (self.task.running || self.task.terminationReason != NSTaskTerminationReasonExit) return ClosureUnconfirmed;
    int code = self.task.terminationStatus;
    return (code == NoProviderWork || code == AllConnectionsClosed) ? code : ClosureUnconfirmed;
}

- (void)stop {
    [self.writer closeAndReturnError:nil];
    self.writer = nil;
    // Closing the parent pipe requests graceful provider teardown. The backend
    // bounds this at 32 seconds plus three seconds for listener retirement.
    if (self.task.running) [self.task waitUntilExit];
    if (self.directory) {
        unlink([self.directory stringByAppendingPathComponent:@"credentials.json"].fileSystemRepresentation);
        rmdir(self.directory.fileSystemRepresentation);
    }
    self.directory = nil;
    self.entry = nil;
}
@end

@interface Controller : NSObject <NSApplicationDelegate, NSWindowDelegate, NSTextFieldDelegate>
@property (strong) NSTextField *minutesField;
@property (strong) NSTextField *costField;
@property (strong) NSTextField *estimateLabel;
@property (strong) NSWindow *window;
@property (strong) NSTextField *status;
@property (strong) NSButton *practice;
@property (strong) NSButton *live;
@property (strong) NSButton *end;
@property (strong) Session *session;
@property (strong) NSTimer *timer;
@property (strong) NSURL *chrome;
@property NSTimeInterval deadline;
@property BOOL paid;
@property BOOL opened;
@property BOOL stopping;
@property BOOL quitting;
@end

@implementation Controller
- (void)applicationDidFinishLaunching:(NSNotification *)note {
    NSRect bounds = NSMakeRect(0, 0, 560, 320);
    self.window = [[NSWindow alloc] initWithContentRect:bounds styleMask:
        NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskMiniaturizable
        backing:NSBackingStoreBuffered defer:NO];
    self.window.title = @"Enny · Real-time Interpreter";
    NSString *build = [NSBundle.mainBundle objectForInfoDictionaryKey:@"CFBundleVersion"];
    if (build.length) self.window.subtitle = [NSString stringWithFormat:@"Build %@", build];
    self.window.delegate = self;
    self.window.releasedWhenClosed = NO;
    NSStackView *stack = [NSStackView stackViewWithViews:@[]];
    stack.orientation = NSUserInterfaceLayoutOrientationVertical;
    stack.alignment = NSLayoutAttributeLeading;
    stack.spacing = 15;
    stack.translatesAutoresizingMaskIntoConstraints = NO;
    NSTextField *heading = [NSTextField labelWithString:@"Open the interpreter"];
    heading.font = [NSFont boldSystemFontOfSize:22];
    [stack addArrangedSubview:heading];
    self.status = [NSTextField wrappingLabelWithString:
        @"A ten-minute rehearsal is free (US$0). Live interpretation uses your own OpenAI key. Review the estimated cost before you approve."];
    [self.status.widthAnchor constraintEqualToConstant:500].active = YES;
    [stack addArrangedSubview:self.status];
    self.practice = [NSButton buttonWithTitle:@"Start free rehearsal" target:self action:@selector(rehearse:)];
    self.practice.keyEquivalent = @"\r";
    self.live = [NSButton buttonWithTitle:@"Start live interpretation · billed by your own OpenAI account" target:self action:@selector(approve:)];
    self.end = [NSButton buttonWithTitle:@"Stop session" target:self action:@selector(stop:)];
    self.end.enabled = NO;
    [stack addArrangedSubview:self.practice];
    [stack addArrangedSubview:self.live];
    [stack addArrangedSubview:self.end];
    [self.window.contentView addSubview:stack];
    [NSLayoutConstraint activateConstraints:@[
        [stack.leadingAnchor constraintEqualToAnchor:self.window.contentView.leadingAnchor constant:30],
        [stack.topAnchor constraintEqualToAnchor:self.window.contentView.topAnchor constant:32]]];
    [self.window center];
    [self.window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
}
- (void)rehearse:(id)sender { [self begin:nil minutes:10 budgetCents:0]; }

- (NSTextField *)addField:(NSString *)label initial:(NSString *)initial toForm:(NSStackView *)form {
    [form addArrangedSubview:[NSTextField labelWithString:label]];
    NSTextField *field = [NSTextField textFieldWithString:initial];
    [field.widthAnchor constraintEqualToConstant:130].active = YES;
    [field setAccessibilityLabel:label];
    field.delegate = self;
    [form addArrangedSubview:field];
    return field;
}

- (void)controlTextDidChange:(NSNotification *)notification {
    NSInteger minutes = 0, cents = 0;
    NSString *problem = approvalError(self.minutesField.stringValue, self.costField.stringValue, &minutes, &cents);
    self.estimateLabel.stringValue = approvalStatus(minutes, problem);
}

- (void)approve:(id)sender {
    if (self.session) return;
    NSAlert *dialog = [[NSAlert alloc] init];
    dialog.messageText = @"Review your OpenAI cost before opening Chrome";
    dialog.informativeText = @"Opening the operator tab costs US$0. When you start live processing in Chrome, OpenAI can charge the account that owns your API key. The local limit controls which connections Enny opens. OpenAI sets the final charge. Enny charges no fee. No managed plan starts.";
    NSButton *cancel = [dialog addButtonWithTitle:@"Cancel"];
    NSButton *approve = [dialog addButtonWithTitle:@"Approve and open"];
    NSStackView *form = [NSStackView stackViewWithViews:@[]];
    form.orientation = NSUserInterfaceLayoutOrientationVertical;
    form.alignment = NSLayoutAttributeLeading;
    form.spacing = 8;
    self.minutesField = [self addField:@"Session length (minutes)" initial:@"60" toForm:form];
    self.costField = [self addField:@"Local cost limit (US dollars)" initial:@"5.00" toForm:form];
    self.estimateLabel = [NSTextField wrappingLabelWithString:estimateText(60)];
    [self.estimateLabel.widthAnchor constraintEqualToConstant:400].active = YES;
    [self.estimateLabel.heightAnchor constraintGreaterThanOrEqualToConstant:54].active = YES;
    [form addArrangedSubview:self.estimateLabel];
    NSSecureTextField *key = [[NSSecureTextField alloc] initWithFrame:NSMakeRect(0, 0, 360, 24)];
    [key.widthAnchor constraintEqualToConstant:360].active = YES;
    key.placeholderString = @"OpenAI API key";
    [key setAccessibilityLabel:@"OpenAI API key"];
    [form addArrangedSubview:key];
    NSMutableArray<NSButton *> *checks = [NSMutableArray array];
    for (NSString *label in @[@"Participants have agreed to live interpretation.",
                               @"I checked the capture route.",
                               @"I checked headphone playback.",
                               @"I reviewed the risk of transcription and translation errors."]) {
        NSButton *check = [NSButton checkboxWithTitle:label target:nil action:nil];
        [checks addObject:check];
        [form addArrangedSubview:check];
    }
    form.frame = NSMakeRect(0, 0, 400, form.fittingSize.height);
    dialog.accessoryView = form;
    dialog.window.initialFirstResponder = self.minutesField;
    approve.keyEquivalent = @"";
    cancel.keyEquivalent = @"\r";
    dialog.window.defaultButtonCell = cancel.cell;
    while ([dialog runModal] == NSAlertSecondButtonReturn) {
        NSInteger minutes = 0, cents = 0;
        NSString *problem = approvalError(self.minutesField.stringValue, self.costField.stringValue, &minutes, &cents);
        if (problem) {
            self.estimateLabel.stringValue = approvalStatus(minutes, problem);
            dialog.messageText = @"Check your session limits";
            continue;
        }
        NSString *value = key.stringValue;
        BOOL valid = value.length >= 16 && value.length <= 4096;
        for (NSUInteger i = 0; valid && i < value.length; i++) {
            unichar c = [value characterAtIndex:i];
            valid = c >= 33 && c <= 126;
        }
        for (NSButton *check in checks) valid = valid && check.state == NSControlStateValueOn;
        if (valid) {
            key.stringValue = @"";
            [self begin:value minutes:minutes budgetCents:cents];
            return;
        }
        dialog.messageText = @"Enter a key and confirm all four checks";
    }
    key.stringValue = @"";
}
- (void)begin:(NSString *)key minutes:(NSInteger)minutes budgetCents:(NSInteger)cents {
    if (self.session) return;
    self.chrome = [NSWorkspace.sharedWorkspace URLForApplicationWithBundleIdentifier:@"com.google.Chrome"];
    if (!self.chrome) {
        self.status.stringValue = @"Install Google Chrome, then start a session. The operator uses Chrome’s shared-tab audio capture.";
        return;
    }
    self.session = [[Session alloc] init];
    self.practice.enabled = self.live.enabled = NO;
    self.end.enabled = YES;
    self.paid = key != nil;
    self.opened = NO;
    self.status.stringValue = @"Starting the local interpreter…";
    if (![self.session startWithKey:key minutes:minutes budgetCents:cents]) {
        [self finish:@"The local interpreter could not start."];
        return;
    }
    self.deadline = NSProcessInfo.processInfo.systemUptime + 15;
    self.timer = [NSTimer scheduledTimerWithTimeInterval:0.15 target:self selector:@selector(check:) userInfo:nil repeats:YES];
}
- (void)check:(NSTimer *)timer {
    if (!self.session.task.running) {
        [self finish:self.opened ? @"The session ended." : @"Startup failed. Check that loopback ports 5180 and 5182 are available."];
        return;
    }
    if (self.opened) return;
    NSURL *url = self.session.entry;
    if (!url) {
        if (NSProcessInfo.processInfo.systemUptime > self.deadline)
            [self finish:@"The local interpreter did not become ready."];
        return;
    }
    self.opened = YES;
    Session *opening = self.session;
    [NSWorkspace.sharedWorkspace openURLs:@[url] withApplicationAtURL:self.chrome
                           configuration:[NSWorkspaceOpenConfiguration configuration]
                       completionHandler:^(NSRunningApplication *app, NSError *error) {
        dispatch_async(dispatch_get_main_queue(), ^{
            if (self.session != opening || self.stopping) return;
            opening.entry = nil;
            if (error || !app) [self finish:@"Chrome could not open the operator tab."];
            else self.status.stringValue = @"The operator tab is open in Chrome. Keep it private. Stop here to end the local service.";
        });
    }];
}
- (void)stop:(id)sender { [self finish:@"Session stopped."]; }
- (void)finish:(NSString *)message {
    if (self.stopping) return;
    [self.timer invalidate];
    self.timer = nil;
    self.stopping = YES;
    self.end.enabled = NO;
    self.status.stringValue = @"Closing the session…";
    Session *closing = self.session;
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        [closing stop];
        dispatch_async(dispatch_get_main_queue(), ^{
            self.session = nil;
            self.stopping = NO;
            self.practice.enabled = self.live.enabled = YES;
            self.status.stringValue = message;
            if (self.paid) {
                int receipt = closing.paidShutdownReceipt;
                NSString *outcome = paidShutdownMessage(receipt);
                // Preserve a useful startup error when the helper never admitted work.
                self.status.stringValue = !self.opened && receipt == NoProviderWork
                    ? [NSString stringWithFormat:@"%@ %@", message, outcome] : outcome;
                if (self.quitting) {
                    NSAlert *alert = [[NSAlert alloc] init];
                    alert.messageText = receipt == ClosureUnconfirmed ? @"Review your live session usage" : @"Your live session ended";
                    alert.informativeText = outcome;
                    [alert runModal];
                }
            }
            if (self.quitting) [NSApp replyToApplicationShouldTerminate:YES];
        });
    });
}
- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)app { return YES; }
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)app {
    if (!self.session) return NSTerminateNow;
    self.quitting = YES;
    [self stop:nil];
    return NSTerminateLater;
}
@end

int main(int argc, const char **argv) {
    @autoreleasepool {
        signal(SIGPIPE, SIG_IGN);
        BOOL rehearsalCheck = argc == 2 && strcmp(argv[1], "--check") == 0;
        BOOL liveCheck = argc > 1 && strcmp(argv[1], "--check-live-config") == 0;
        BOOL expiryCheck = argc == 2 && strcmp(argv[1], "--check-idle-expiry") == 0;
        if (rehearsalCheck || liveCheck || expiryCheck) {
            NSInteger minutes = 10, cents = 0;
            if (liveCheck) {
                if (argc != 4) return 2;
                NSString *error = approvalError([NSString stringWithUTF8String:argv[2]],
                                                [NSString stringWithUTF8String:argv[3]], &minutes, &cents);
                if (error) {
                    fprintf(stderr, "%s\n", error.UTF8String);
                    return 2;
                }
                printf("%s Local limit: %s.\n", estimateText(minutes).UTF8String, dollars(cents).UTF8String);
            }
            if (expiryCheck) { minutes = 1; cents = 7; }
            Session *check = [[Session alloc] init];
            check.checking = YES;
            BOOL ready = NO;
            NSString *key = (liveCheck || expiryCheck) ? @"fictional-key-never-transmitted" : nil;
            if ([check startWithKey:key minutes:minutes budgetCents:cents]) {
                for (unsigned i = 0; i < 150 && check.task.running; i++) {
                    if (check.entry) { ready = YES; break; }
                    [NSThread sleepForTimeInterval:0.1];
                }
            }
            if (ready && expiryCheck) {
                // Leave the pipe open through the one-minute meeting window and bootstrap grace.
                // The check supplies only an inert key and never opens a browser or audio route.
                NSTimeInterval limit = NSProcessInfo.processInfo.systemUptime + 220;
                while (check.task.running && NSProcessInfo.processInfo.systemUptime < limit)
                    [NSThread sleepForTimeInterval:0.1];
                ready = !check.task.running;
            }
            [check stop];
            if (key) {
                int receipt = check.paidShutdownReceipt;
                puts(paidShutdownMessage(receipt).UTF8String);
                ready = ready && receipt == NoProviderWork;
            } else {
                ready = ready && check.task.terminationReason == NSTaskTerminationReasonExit && check.task.terminationStatus == 0;
            }
            puts(ready ? "Packaged session started and stopped." : "Packaged session check failed.");
            return ready ? 0 : 1;
        }
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyRegular];
        Controller *controller = [[Controller alloc] init];
        app.delegate = controller;
        [app run];
        (void)controller.window;
        return 0;
    }
}
