//! Native window icons and the development build's macOS Dock icon.

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::NonSendMarker;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::window::WindowCreated;
use bevy::winit::WINIT_WINDOWS;
use winit::window::Icon;

fn window_icon() -> Icon {
    let image = Image::from_buffer(
        include_bytes!("../assets/icons/bloq-256.png"),
        ImageType::Extension("png"),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::default(),
        RenderAssetUsages::MAIN_WORLD,
    )
    .expect("embedded app icon is a PNG")
    .convert(TextureFormat::Rgba8UnormSrgb)
    .expect("app icon converts to RGBA8");
    Icon::from_rgba(
        image.data.expect("app icon has pixel data"),
        image.texture_descriptor.size.width,
        image.texture_descriptor.size.height,
    )
    .expect("app icon has complete RGBA pixels")
}

pub(crate) fn set_window_icons(
    mut created: MessageReader<WindowCreated>,
    _main_thread: NonSendMarker,
) {
    WINIT_WINDOWS.with_borrow(|windows| {
        for event in created.read() {
            if let Some(window) = windows.get_window(event.window) {
                let icon = window_icon();
                #[cfg(target_os = "windows")]
                {
                    use winit::platform::windows::WindowExtWindows;
                    window.set_taskbar_icon(Some(icon.clone()));
                }
                window.set_window_icon(Some(icon));
            }
        }
    });
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "AppKit's icon setter requires a non-null NSImage"
)]
pub(crate) fn set_dock_icon(_main_thread: NonSendMarker) {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let main_thread = MainThreadMarker::new().expect("Dock icon runs on the main thread");
    let data = NSData::with_bytes(include_bytes!("../assets/icons/bloq.icns"));
    let image = NSImage::initWithData(NSImage::alloc(), &data)
        .expect("embedded app icon is a valid ICNS image");
    // SAFETY: The image is non-null and AppKit is called on the main thread.
    unsafe { NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image)) };
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_images_decode_for_native_icons() {
        super::window_icon();
        #[cfg(target_os = "macos")]
        {
            use objc2::AnyThread;
            use objc2_app_kit::NSImage;
            use objc2_foundation::NSData;

            let data = NSData::with_bytes(include_bytes!("../assets/icons/bloq.icns"));
            assert!(NSImage::initWithData(NSImage::alloc(), &data).is_some());
        }
    }
}
