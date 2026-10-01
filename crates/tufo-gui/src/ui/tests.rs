use std::rc::Rc;

use slint::{
    ComponentHandle as _, LogicalPosition, PhysicalSize, PlatformError,
    platform::{
        Platform, PointerEventButton, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};

mod fixture {
    slint::slint! {
        import { ScrollThumb } from "../../ui/widgets.slint";

        export component ScrollWindow inherits Window {
            width: 200px;
            height: 200px;
            in property <length> content-height: 1000px;
            in property <length> scrollbar-inset;
            in-out property <length> offset <=> flick.content-y;
            flick := Flickable {
                mouse-drag-pan-enabled: false;
                content-width: self.width;
                content-height: root.content-height;
                Rectangle { height: root.content-height; }
            }
            ScrollThumb {
                x: parent.width - self.width;
                y: root.scrollbar-inset;
                height: parent.height - root.scrollbar-inset * 2;
                content-height: flick.content-height;
                viewport-height: flick.height;
                content-y <=> flick.content-y;
                show: true;
            }
        }
    }
}

struct ScrollPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for ScrollPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn window() -> anyhow::Result<fixture::ScrollWindow> {
    let backend = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(ScrollPlatform(backend.clone())))?;
    backend.set_size(PhysicalSize::new(200, 200));
    let window = fixture::ScrollWindow::new()?;
    window.show()?;
    Ok(window)
}

fn press(window: &fixture::ScrollWindow, x: f32, y: f32) -> anyhow::Result<()> {
    window
        .window()
        .dispatch_event_with_result(WindowEvent::PointerPressed {
            position: LogicalPosition::new(x, y),
            button: PointerEventButton::Left,
        })?;
    Ok(())
}

fn move_pointer(window: &fixture::ScrollWindow, x: f32, y: f32) -> anyhow::Result<()> {
    window
        .window()
        .dispatch_event_with_result(WindowEvent::PointerMoved {
            position: LogicalPosition::new(x, y),
        })?;
    Ok(())
}

fn release(window: &fixture::ScrollWindow, x: f32, y: f32) -> anyhow::Result<()> {
    window
        .window()
        .dispatch_event_with_result(WindowEvent::PointerReleased {
            position: LogicalPosition::new(x, y),
            button: PointerEventButton::Left,
        })?;
    Ok(())
}

fn assert_offset(window: &fixture::ScrollWindow, expected: f32) {
    let offset = window.get_offset();
    assert!(
        (offset - expected).abs() < 0.1,
        "offset {offset}, expected {expected}"
    );
}

#[test]
fn scrollbar_track_click_moves_the_content() -> anyhow::Result<()> {
    let window = window()?;
    press(&window, 195.0, 100.0)?;
    release(&window, 195.0, 100.0)?;
    assert_offset(&window, -400.0);
    Ok(())
}

#[test]
fn scrollbar_drag_keeps_the_grab_point_and_clamps_to_both_ends() -> anyhow::Result<()> {
    let window = window()?;
    press(&window, 195.0, 10.0)?;
    assert_offset(&window, 0.0);
    move_pointer(&window, 150.0, 90.0)?;
    assert_offset(&window, -400.0);
    move_pointer(&window, 150.0, 250.0)?;
    assert_offset(&window, -800.0);
    move_pointer(&window, 150.0, -20.0)?;
    assert_offset(&window, 0.0);
    release(&window, 150.0, -20.0)?;
    move_pointer(&window, 195.0, 100.0)?;
    assert_offset(&window, 0.0);
    Ok(())
}

#[test]
fn scrollbar_wheel_scrolls_the_content() -> anyhow::Result<()> {
    let window = window()?;
    window
        .window()
        .dispatch_event_with_result(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(195.0, 100.0),
            delta_x: 0.0,
            delta_y: -80.0,
        })?;
    assert_offset(&window, -80.0);
    Ok(())
}

#[test]
fn inset_scrollbar_reaches_the_content_end() -> anyhow::Result<()> {
    let window = window()?;
    window.set_scrollbar_inset(12.0);
    press(&window, 195.0, 187.0)?;
    release(&window, 195.0, 187.0)?;
    assert_offset(&window, -800.0);
    Ok(())
}

#[test]
fn scrollbar_does_not_scroll_content_that_fits() -> anyhow::Result<()> {
    let window = window()?;
    window.set_content_height(200.0);
    press(&window, 195.0, 100.0)?;
    move_pointer(&window, 195.0, 190.0)?;
    release(&window, 195.0, 190.0)?;
    assert_offset(&window, 0.0);
    Ok(())
}

#[test]
fn dragging_page_content_does_not_pan() -> anyhow::Result<()> {
    let window = window()?;
    press(&window, 80.0, 150.0)?;
    move_pointer(&window, 80.0, 50.0)?;
    release(&window, 80.0, 50.0)?;
    assert_offset(&window, 0.0);
    Ok(())
}
