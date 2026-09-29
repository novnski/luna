//! Keep the running macOS application icon aligned with LUNA's resolved theme.

use crate::theme::Appearance;

#[cfg(target_os = "macos")]
pub(crate) fn sync(appearance: Appearance) {
    use std::cell::RefCell;

    use objc2::{AnyThread, MainThreadMarker, rc::Retained};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    #[derive(Default)]
    struct Icons {
        light: Option<Retained<NSImage>>,
        dark: Option<Retained<NSImage>>,
        current: Option<Appearance>,
    }

    thread_local! {
        static ICONS: RefCell<Icons> = RefCell::new(Icons::default());
    }

    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("application icon update requires the main thread");
        return;
    };
    ICONS.with_borrow_mut(|icons| {
        if icons.current == Some(appearance) {
            return;
        }
        let (image, bytes) = match appearance {
            Appearance::Light => (
                &mut icons.light,
                include_bytes!("../assets/app-icons/light.icns").as_slice(),
            ),
            Appearance::Dark => (
                &mut icons.dark,
                include_bytes!("../assets/app-icons/dark.icns").as_slice(),
            ),
        };
        if image.is_none() {
            *image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes));
        }
        let Some(image) = image.as_ref() else {
            tracing::warn!(?appearance, "unable to decode embedded application icon");
            return;
        };
        // AppKit retains the image. Both variants stay cached for later switches.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(image)) };
        icons.current = Some(appearance);
    });
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn sync(_appearance: Appearance) {}
