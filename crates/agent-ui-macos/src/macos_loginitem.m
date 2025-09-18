#import <Foundation/Foundation.h>
#import <ServiceManagement/ServiceManagement.h>
#import <objc/message.h>

bool ripor_loginitem_register(const char *bundle_id) {
    @autoreleasepool {
        if (@available(macOS 13.0, *)) {
            NSString *bid = [NSString stringWithUTF8String:bundle_id];
            Class cls = objc_getClass("SMAppService");
            if (!cls) return false;
            SEL selLogin = sel_registerName("loginItemWithIdentifier:");
            id svc = ((id(*)(id, SEL, id))objc_msgSend)((id)cls, selLogin, bid);
            if (!svc) return false;
            SEL selReg = sel_registerName("registerAndReturnError:");
            BOOL ok = ((BOOL(*)(id, SEL, void*))objc_msgSend)(svc, selReg, NULL);
            return ok == YES;
        }
        return false;
    }
}

bool ripor_loginitem_unregister(const char *bundle_id) {
    @autoreleasepool {
        if (@available(macOS 13.0, *)) {
            NSString *bid = [NSString stringWithUTF8String:bundle_id];
            Class cls = objc_getClass("SMAppService");
            if (!cls) return false;
            SEL selLogin = sel_registerName("loginItemWithIdentifier:");
            id svc = ((id(*)(id, SEL, id))objc_msgSend)((id)cls, selLogin, bid);
            if (!svc) return false;
            SEL selUnreg = sel_registerName("unregisterAndReturnError:");
            BOOL ok = ((BOOL(*)(id, SEL, void*))objc_msgSend)(svc, selUnreg, NULL);
            return ok == YES;
        }
        return false;
    }
}

// Readback helper: report whether the given LoginItem identifier is registered/enabled
bool ripor_loginitem_is_registered(const char *bundle_id) {
    @autoreleasepool {
        if (@available(macOS 13.0, *)) {
            NSString *bid = [NSString stringWithUTF8String:bundle_id];
            Class cls = objc_getClass("SMAppService");
            if (!cls) return false;
            SEL selLogin = sel_registerName("loginItemWithIdentifier:");
            id svc = ((id(*)(id, SEL, id))objc_msgSend)((id)cls, selLogin, bid);
            if (!svc) return false;
            // Try -status (SMAppServiceStatus)
            SEL selStatus = sel_registerName("status");
            if ([svc respondsToSelector:selStatus]) {
                @try {
                    NSInteger status = ((NSInteger(*)(id, SEL))objc_msgSend)(svc, selStatus);
                    return status == 2; // SMAppServiceStatusEnabled
                } @catch (NSException *ex) {
                    // fallthrough
                }
            }
            // Fallback: -isEnabled
            SEL selEnabled = sel_registerName("isEnabled");
            if ([svc respondsToSelector:selEnabled]) {
                @try {
                    BOOL enabled = ((BOOL(*)(id, SEL))objc_msgSend)(svc, selEnabled);
                    return enabled == YES;
                } @catch (NSException *ex) {
                    return false;
                }
            }
        }
        return false;
    }
}
