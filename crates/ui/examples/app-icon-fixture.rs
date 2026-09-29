//! Production appearance controller -> native AppKit application icon E2E.

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use gpui::App;
    use luna_ui::{appearance, settings, theme::Appearance, theme_library};
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};

    fn pixels(tiff: &Path) -> image::RgbaImage {
        let png = tiff.with_extension("png");
        let result = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "png"])
            .arg(tiff)
            .arg("--out")
            .arg(&png)
            .output()
            .expect("native TIFF conversion");
        assert!(
            result.status.success(),
            "unable to normalize native icon TIFF"
        );
        image::open(png).unwrap().to_rgba8()
    }

    fn premultiply(pixel: &image::Rgba<u8>) -> [i16; 4] {
        let alpha = u16::from(pixel[3]);
        [
            ((u16::from(pixel[0]) * alpha + 127) / 255) as i16,
            ((u16::from(pixel[1]) * alpha + 127) / 255) as i16,
            ((u16::from(pixel[2]) * alpha + 127) / 255) as i16,
            alpha as i16,
        ]
    }

    fn readback() -> (usize, Vec<u8>) {
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        let image = app.applicationIconImage().expect("native application icon");
        let pointer = (&*image as *const NSImage) as usize;
        let data = image.TIFFRepresentation().expect("native icon TIFF");
        (pointer, data.to_vec())
    }

    fn expected(bytes: &[u8]) -> Vec<u8> {
        NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes))
            .expect("decodable ICNS asset")
            .TIFFRepresentation()
            .unwrap()
            .to_vec()
    }

    let output = PathBuf::from(std::env::args().nth(1).expect("evidence directory"));
    std::fs::create_dir_all(&output)?;
    let profile = tempfile::tempdir()?;
    let data_dir = profile.path().to_path_buf();
    let light = expected(include_bytes!("../assets/app-icons/light.icns"));
    let dark = expected(include_bytes!("../assets/app-icons/dark.icns"));
    std::fs::write(output.join("expected-light.tiff"), &light)?;
    std::fs::write(output.join("expected-dark.tiff"), &dark)?;
    assert_ne!(light, dark, "light and dark artwork must differ");
    let light_pixels = pixels(&output.join("expected-light.tiff"));
    let dark_pixels = pixels(&output.join("expected-dark.tiff"));

    gpui_platform::headless().run(move |cx: &mut App| {
        let defaults = settings::UiSettings::default();
        settings::init(defaults.clone(), data_dir.clone(), cx);
        theme_library::init(data_dir, cx);
        appearance::init(
            appearance::AppearanceMode::Light,
            defaults.theme_selection,
            defaults.accent,
            defaults.surface,
            cx,
        );

        let mut results = Vec::new();
        for (name, mode, system, wanted) in [
            ("startup-light", appearance::AppearanceMode::Light, Appearance::Dark, Appearance::Light),
            ("pinned-dark", appearance::AppearanceMode::Dark, Appearance::Light, Appearance::Dark),
            ("pinned-light", appearance::AppearanceMode::Light, Appearance::Dark, Appearance::Light),
            ("system-dark", appearance::AppearanceMode::System, Appearance::Dark, Appearance::Dark),
            ("system-light", appearance::AppearanceMode::System, Appearance::Light, Appearance::Light),
        ] {
            cx.global_mut::<appearance::AppearanceState>().system = system;
            appearance::set_mode(mode, cx);
            appearance::apply(cx);
            let (pointer, bytes) = readback();
            let expected = if wanted == Appearance::Light { &light_pixels } else { &dark_pixels };
            let tiff = output.join(format!("{name}.tiff"));
            std::fs::write(&tiff, &bytes).unwrap();
            let actual = pixels(&tiff);
            assert_eq!(actual.dimensions(), expected.dimensions(), "{name}: icon dimensions");
            let max_delta = actual.pixels().zip(expected.pixels())
                .flat_map(|(a, b)| premultiply(a).into_iter().zip(premultiply(b)))
                .map(|(a, b)| (a - b).abs()).max().unwrap();
            // AppKit promotes application icons to extended-range TIFFs. Compare
            // visible pixels, allowing only its measured 2/255 quantization loss.
            assert!(max_delta <= 2, "{name}: visible pixel difference {max_delta}");
            appearance::apply(cx);
            assert_eq!(readback().0, pointer, "{name}: no-op must reuse the native image");
            std::fs::write(output.join(format!("{name}.tiff")), &bytes).unwrap();
            results.push(serde_json::json!({
                "case": name, "mode": mode, "system": format!("{system:?}"),
                "resolved": format!("{wanted:?}"), "sha256": format!("{:x}", Sha256::digest(&bytes)),
                "native_image_reused": true, "max_pixel_delta": max_delta
            }));
        }
        std::fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"ok": true, "cases": results})).unwrap(),
        ).unwrap();
        println!("native application icon E2E: 5 cases passed; {}", output.display());
        cx.quit();
    });
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    panic!("AppKit icon E2E requires macOS");
}
