// Guard for the _NSTouchBarFinderObservation -invalidate swizzle.
//
// The @try/@catch must live in C: Objective-C exceptions unwind through
// every frame between the thrower and the catcher, and frames emitted by
// rustc cannot be unwound through in `panic = "abort"` builds — release
// builds abort before any Rust-side catch helper can see the exception.
// C frames always carry unwind tables, so catching here works in every
// profile. See src/macos.rs `guard_touch_bar_finder` and emilk/egui#2768.

#import <Foundation/NSException.h>
#import <objc/runtime.h>

/// Calls `imp(object, selector)`. Returns false when the call threw an
/// `NSRangeException` (the stale-observer removal AppKit hits on window
/// teardown); any other exception is a real failure and is rethrown.
bool zapfast_call_swallowing_range_error(IMP imp, id object, SEL selector) {
    @try {
        ((void (*)(id, SEL)) imp)(object, selector);
        return true;
    } @catch (NSException *exception) {
        if ([exception.name isEqualToString:@"NSRangeException"]) {
            return false;
        }
        @throw;
    }
}
