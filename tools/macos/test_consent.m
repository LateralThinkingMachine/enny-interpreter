// Drive the real AppKit controls. Intercept session starts before any service or browser opens.
#define main EnnyLauncherMain
#import "Interpreter.m"
#undef main
@interface ProbeController : Controller
@property NSInteger startCount;
@property NSInteger acceptedMinutes;
@property NSInteger acceptedBudget;
@property BOOL hadKey;
@end
@implementation ProbeController
- (void)begin:(NSString *)key minutes:(NSInteger)minutes budgetCents:(NSInteger)cents {
    self.hadKey = key != nil;
    self.acceptedMinutes = minutes;
    self.acceptedBudget = cents;
    self.startCount += 1;
}
@end
static void check(BOOL ok, const char *observation) {
    if (!ok) {
        fprintf(stderr, "Failed native consent check: %s\n", observation);
        exit(2);
    }
}
static BOOL visibleText(NSView *view, NSString *needle) {
    if ([view isKindOfClass:NSTextField.class] && [((NSTextField *)view).stringValue containsString:needle])
        return YES;
    for (NSView *child in view.subviews) if (visibleText(child, needle)) return YES;
    return NO;
}
static NSButton *findButton(NSView *view, NSString *title) {
    if ([view isKindOfClass:NSButton.class] && [((NSButton *)view).title isEqualToString:title])
        return (NSButton *)view;
    for (NSView *child in view.subviews) {
        NSButton *button = findButton(child, title);
        if (button) return button;
    }
    return nil;
}
// Drive the production Stop and timer paths with a real child exit and a
// private pipe; the child simulates a receipt without any provider or audio.
static void checkExit(Controller *controller, int code, BOOL manual, NSString *expected) {
    Session *session = [Session new];
    NSPipe *pipe = [NSPipe pipe];
    session.writer = pipe.fileHandleForWriting;
    session.task = [NSTask new];
    session.task.executableURL = [NSURL fileURLWithPath:@"/bin/sh"];
    session.task.arguments = @[@"-c", manual ? @"cat >/dev/null; exit \"$1\"" : @"exit \"$1\"",
                               @"receipt", [NSString stringWithFormat:@"%d", code]];
    session.task.standardInput = pipe;
    check([session.task launchAndReturnError:nil], "simulated helper starts");
    session.launched = YES;
    controller.session = session;
    controller.paid = YES;
    controller.opened = YES;
    if (manual) [controller stop:nil];
    else {
        [session.task waitUntilExit];
        [controller check:nil];
    }
    NSTimeInterval end = NSProcessInfo.processInfo.systemUptime + 3;
    while (controller.session && NSProcessInfo.processInfo.systemUptime < end)
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
    check(controller.session == nil, "local session retires");
    check([controller.status.stringValue isEqualToString:expected], "paid Stop status matches the provider receipt");
}

int main(void) {
    @autoreleasepool {
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
        [app finishLaunching];
        ProbeController *controller = [ProbeController new];
        [controller applicationDidFinishLaunching:[NSNotification notificationWithName:NSApplicationDidFinishLaunchingNotification object:app]];
        [controller.window.contentView layoutSubtreeIfNeeded];
        check([controller.practice.title isEqualToString:@"Start free rehearsal"], "native free action");
        check([controller.live.title isEqualToString:@"Start live interpretation · billed by your own OpenAI account"], "native paid action");
        check([controller.practice.keyEquivalent isEqualToString:@"\r"], "Return defaults to rehearsal");
        NSRect box = [controller.window.contentView convertRect:controller.live.bounds fromView:controller.live];
        check(NSContainsRect(controller.window.contentView.bounds, box) && box.size.width >= controller.live.intrinsicContentSize.width, "paid action fits window");
        [controller.practice performClick:nil];
        check(controller.startCount == 1 && !controller.hadKey && controller.acceptedBudget == 0 && controller.acceptedMinutes == 10, "free action launches zero-budget rehearsal");
        NSNotification *change = [NSNotification notificationWithName:NSControlTextDidChangeNotification object:nil];
        __block int step = 0;
        NSTimer *inspect = [NSTimer timerWithTimeInterval:0.2 repeats:YES block:^(NSTimer *timer) {
            NSWindow *dialog = [app modalWindow];
            if (!dialog) return;
            [dialog.contentView layoutSubtreeIfNeeded];
            NSButton *cancel = findButton(dialog.contentView, @"Cancel");
            NSButton *approve = findButton(dialog.contentView, @"Approve and open");
            if (step++ == 0) {
                check(cancel && approve, "separate confirm and cancel");
                check(dialog.defaultButtonCell == cancel.cell && [cancel.keyEquivalent isEqualToString:@"\r"],
                      "Return defaults to Cancel in paid dialog");
                check(visibleText(dialog.contentView, @"Opening the operator tab costs US$0."), "operator open is free");
                check(visibleText(dialog.contentView, @"OpenAI can charge the account that owns your API key"), "payer and billing trigger in accessible dialog text");
                check(visibleText(dialog.contentView, @"Enny charges no fee."), "Enny fee disclosed");
                check(visibleText(dialog.contentView, @"No managed plan starts."), "managed fee excluded");
                check([controller.estimateLabel.stringValue containsString:@"US$4.02 for 60 minutes"], "one hour shows correct estimate");
                check([controller.costField.stringValue isEqualToString:@"5.00"] && [controller.costField.accessibilityLabel isEqualToString:@"Local cost limit (US dollars)"], "local limit separate from price");

                controller.minutesField.stringValue = @"17";
                controller.costField.stringValue = @"2";
                [controller controlTextDidChange:change];
                check([controller.estimateLabel.stringValue containsString:@"US$1.14 for 17 minutes"], "quote responds to duration changes");
                controller.costField.stringValue = @"1.13";
                [controller controlTextDidChange:change];
                check([controller.estimateLabel.stringValue containsString:@"Choose a local limit of US$1.14 or more."], "underfunded limit gives actionable error");
                [approve performClick:nil];
            } else {
                check(visibleText(dialog.contentView, @"Check your session limits"), "invalid approval stays at local consent");
                controller.minutesField.stringValue = @"61";
                [controller controlTextDidChange:change];
                check([controller.estimateLabel.stringValue isEqualToString:@"Enter a whole number of minutes from 1 to 60."], "excess duration is refused");
                [timer invalidate];
                [cancel performClick:nil];
            }
        }];
        [[NSRunLoop currentRunLoop] addTimer:inspect forMode:NSModalPanelRunLoopMode];
        [controller approve:nil];
        check(step == 2 && controller.startCount == 1 && !controller.session, "cancel after invalid approval starts no live session");
        for (int manual = 0; manual <= 1; manual++) {
            checkExit(controller, 20, manual, @"Local audio stopped. Enny did not start any OpenAI work.");
            checkExit(controller, 21, manual, @"Local audio stopped. OpenAI confirmed that all admitted connections closed.");
            checkExit(controller, 22, manual, @"Local audio stopped. OpenAI closure is unconfirmed. Check usage in your OpenAI account.");
            checkExit(controller, 0, manual, @"Local audio stopped. OpenAI closure is unconfirmed. Check usage in your OpenAI account.");
        }
        [controller.window orderOut:nil];
        puts("Verified native billing labels, estimate changes, keyboard defaults, invalid approval and Cancel, then simulated manual and natural shutdown receipts.");
    }
    return 0;
}
