//! Protobuf mappings for the browser host's pages and views.

use std::num::NonZeroU32;

use tonic::Status;
use tufo_models::browser::{
    BrowserTab, BrowserTabs, PageAddress, PageFailure, PageLoadError, TabHistory, TabId,
};

use super::{InvalidMessage, invalid, required};
use crate::{
    browser::{
        KeyInput, KeyModifiers, MouseButton, PixelRectangle, PointerShape, ScaleFactor, ViewFrame,
        ViewInput, ViewOutput, ViewPoint, ViewSize,
    },
    pb,
};

// Requests the browser host parses.

pub fn tab_id(id: u32) -> Result<TabId, Status> {
    NonZeroU32::new(id)
        .map(TabId::new)
        .ok_or_else(|| invalid("tab_id", "must be non-zero"))
}

pub fn open_tab_request(
    request: &pb::browser::OpenTabRequest,
) -> Result<Option<PageAddress>, Status> {
    request
        .address
        .as_deref()
        .map(|address| page_address(address, "address"))
        .transpose()
}

pub fn navigate_tab_request(
    request: &pb::browser::NavigateTabRequest,
) -> Result<(TabId, PageAddress), Status> {
    Ok((
        tab_id(request.tab_id)?,
        page_address(&request.address, "address")?,
    ))
}

pub fn view_browser_request(request: pb::browser::ViewBrowserRequest) -> Result<ViewInput, Status> {
    use pb::browser::view_browser_request::Input;

    Ok(match required("input", request.input)? {
        Input::Size(size) => {
            ViewInput::Resize(view_size(size).map_err(|reason| invalid("size", reason))?)
        }
        Input::PointerMoved(moved) => ViewInput::PointerMoved {
            position: ViewPoint {
                x: moved.x,
                y: moved.y,
            },
            modifiers: key_modifiers(moved.modifiers),
        },
        Input::PointerLeft(_) => ViewInput::PointerLeft,
        Input::PointerButton(button) => ViewInput::PointerButton {
            position: ViewPoint {
                x: button.x,
                y: button.y,
            },
            button: mouse_button(button.button)
                .ok_or_else(|| invalid("button", "must be left, middle, or right"))?,
            pressed: button.pressed,
            click_count: u8::try_from(button.click_count.clamp(1, 3)).unwrap_or(1),
            modifiers: key_modifiers(button.modifiers),
        },
        Input::PointerWheel(wheel) => {
            if !wheel.delta_x.is_finite() || !wheel.delta_y.is_finite() {
                return Err(invalid("pointer_wheel", "deltas must be finite"));
            }
            ViewInput::PointerWheel {
                position: ViewPoint {
                    x: wheel.x,
                    y: wheel.y,
                },
                delta_x: wheel.delta_x,
                delta_y: wheel.delta_y,
                modifiers: key_modifiers(wheel.modifiers),
            }
        }
        Input::Key(key) => ViewInput::Key(KeyInput {
            key_code: key.key_code,
            text: key.text,
            pressed: key.pressed,
            repeat: key.repeat,
            modifiers: key_modifiers(key.modifiers),
        }),
        Input::Focus(focus) => ViewInput::Focus(focus.focused),
        Input::Text(text) => ViewInput::InsertText(text.text),
    })
}

// Responses the browser host builds.

#[must_use]
pub fn watch_browser_response(tabs: &BrowserTabs) -> pb::browser::WatchBrowserResponse {
    pb::browser::WatchBrowserResponse {
        tabs: tabs.tabs.iter().map(tab_message).collect(),
        selected_tab_id: tabs.selected.map(|id| id.into_inner().get()),
    }
}

#[must_use]
pub fn view_browser_response(output: ViewOutput) -> pb::browser::ViewBrowserResponse {
    use pb::browser::view_browser_response::Output;

    let output = match output {
        ViewOutput::Frame(frame) => {
            let rectangle = frame.rectangle();
            Output::Frame(pb::browser::ViewFrame {
                width: frame.width(),
                height: frame.height(),
                x: rectangle.x,
                y: rectangle.y,
                rectangle_width: rectangle.width,
                rectangle_height: rectangle.height,
                pixels: frame.into_pixels(),
            })
        }
        ViewOutput::PointerShape(shape) => Output::PointerShape(pb::browser::PointerShapeChange {
            shape: pointer_shape_message(shape).into(),
        }),
    };
    pb::browser::ViewBrowserResponse {
        output: Some(output),
    }
}

// Requests the view builds.

#[must_use]
pub fn navigate_tab_message(tab: TabId, address: &PageAddress) -> pb::browser::NavigateTabRequest {
    pb::browser::NavigateTabRequest {
        tab_id: tab.into_inner().get(),
        address: address.as_str().to_owned(),
    }
}

#[must_use]
pub fn open_tab_message(address: Option<&PageAddress>) -> pb::browser::OpenTabRequest {
    pb::browser::OpenTabRequest {
        address: address.map(|address| address.as_str().to_owned()),
    }
}

#[must_use]
pub fn view_browser_message(input: &ViewInput) -> pb::browser::ViewBrowserRequest {
    use pb::browser::view_browser_request::Input;

    let input = match input {
        ViewInput::Resize(size) => Input::Size(pb::browser::ViewSize {
            width: size.width().get(),
            height: size.height().get(),
            scale_factor: size.scale_factor().into_inner(),
        }),
        ViewInput::PointerMoved {
            position,
            modifiers,
        } => Input::PointerMoved(pb::browser::PointerMoved {
            x: position.x,
            y: position.y,
            modifiers: Some(key_modifiers_message(*modifiers)),
        }),
        ViewInput::PointerLeft => Input::PointerLeft(pb::browser::PointerLeft {}),
        ViewInput::PointerButton {
            position,
            button,
            pressed,
            click_count,
            modifiers,
        } => Input::PointerButton(pb::browser::PointerButton {
            x: position.x,
            y: position.y,
            button: mouse_button_message(*button).into(),
            pressed: *pressed,
            click_count: u32::from(*click_count),
            modifiers: Some(key_modifiers_message(*modifiers)),
        }),
        ViewInput::PointerWheel {
            position,
            delta_x,
            delta_y,
            modifiers,
        } => Input::PointerWheel(pb::browser::PointerWheel {
            x: position.x,
            y: position.y,
            delta_x: *delta_x,
            delta_y: *delta_y,
            modifiers: Some(key_modifiers_message(*modifiers)),
        }),
        ViewInput::Key(key) => Input::Key(pb::browser::KeyInput {
            key_code: key.key_code,
            text: key.text.clone(),
            pressed: key.pressed,
            repeat: key.repeat,
            modifiers: Some(key_modifiers_message(key.modifiers)),
        }),
        ViewInput::Focus(focused) => Input::Focus(pb::browser::ViewFocus { focused: *focused }),
        ViewInput::InsertText(text) => {
            Input::Text(pb::browser::TextInsertion { text: text.clone() })
        }
    };
    pb::browser::ViewBrowserRequest { input: Some(input) }
}

// Responses the view decodes.

pub fn opened_tab(response: pb::browser::OpenTabResponse) -> Result<TabId, InvalidMessage> {
    NonZeroU32::new(response.tab_id)
        .map(TabId::new)
        .ok_or(InvalidMessage { field: "tab_id" })
}

pub fn browser_tabs(
    response: pb::browser::WatchBrowserResponse,
) -> Result<BrowserTabs, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    let tabs = response
        .tabs
        .into_iter()
        .map(browser_tab)
        .collect::<Result<Vec<_>, _>>()?;
    let selected = response
        .selected_tab_id
        .map(|id| {
            NonZeroU32::new(id)
                .map(TabId::new)
                .filter(|id| tabs.iter().any(|tab| tab.id == *id))
                .ok_or(invalid("selected_tab_id"))
        })
        .transpose()?;
    Ok(BrowserTabs { tabs, selected })
}

pub fn view_output(
    response: pb::browser::ViewBrowserResponse,
) -> Result<ViewOutput, InvalidMessage> {
    use pb::browser::view_browser_response::Output;

    let invalid = |field| InvalidMessage { field };
    Ok(match response.output.ok_or(invalid("output"))? {
        Output::Frame(frame) => ViewOutput::Frame(
            ViewFrame::try_new(
                frame.width,
                frame.height,
                PixelRectangle {
                    x: frame.x,
                    y: frame.y,
                    width: frame.rectangle_width,
                    height: frame.rectangle_height,
                },
                frame.pixels,
            )
            .map_err(|_| invalid("frame"))?,
        ),
        Output::PointerShape(change) => {
            ViewOutput::PointerShape(pointer_shape(change.shape).ok_or(invalid("pointer_shape"))?)
        }
    })
}

// Shared conversions.

fn page_address(text: &str, field: &str) -> Result<PageAddress, Status> {
    PageAddress::parse(text).map_err(|error| invalid(field, error))
}

fn view_size(size: pb::browser::ViewSize) -> Result<ViewSize, &'static str> {
    let width = NonZeroU32::new(size.width).ok_or("width must be non-zero")?;
    let height = NonZeroU32::new(size.height).ok_or("height must be non-zero")?;
    let scale_factor =
        ScaleFactor::try_new(size.scale_factor).map_err(|_| "scale_factor must be 0.5 to 4")?;
    ViewSize::try_new(width, height, scale_factor).map_err(|_| "the view is too large")
}

fn key_modifiers(message: Option<pb::browser::KeyModifiers>) -> KeyModifiers {
    let message = message.unwrap_or_default();
    KeyModifiers {
        shift: message.shift,
        control: message.control,
        alt: message.alt,
        meta: message.meta,
    }
}

fn key_modifiers_message(modifiers: KeyModifiers) -> pb::browser::KeyModifiers {
    pb::browser::KeyModifiers {
        shift: modifiers.shift,
        control: modifiers.control,
        alt: modifiers.alt,
        meta: modifiers.meta,
    }
}

fn mouse_button(button: i32) -> Option<MouseButton> {
    match pb::browser::MouseButton::try_from(button) {
        Ok(pb::browser::MouseButton::Left) => Some(MouseButton::Left),
        Ok(pb::browser::MouseButton::Middle) => Some(MouseButton::Middle),
        Ok(pb::browser::MouseButton::Right) => Some(MouseButton::Right),
        Ok(pb::browser::MouseButton::Unspecified) | Err(_) => None,
    }
}

fn mouse_button_message(button: MouseButton) -> pb::browser::MouseButton {
    match button {
        MouseButton::Left => pb::browser::MouseButton::Left,
        MouseButton::Middle => pb::browser::MouseButton::Middle,
        MouseButton::Right => pb::browser::MouseButton::Right,
    }
}

fn tab_message(tab: &BrowserTab) -> pb::browser::BrowserTab {
    pb::browser::BrowserTab {
        tab_id: tab.id.into_inner().get(),
        address: tab.address.clone(),
        title: tab.title.clone(),
        loading: tab.loading,
        can_go_back: tab.history.can_go_back,
        can_go_forward: tab.history.can_go_forward,
        audible: tab.audible,
        failure: tab
            .failure
            .map(|failure| page_failure_message(failure).into()),
    }
}

fn browser_tab(message: pb::browser::BrowserTab) -> Result<BrowserTab, InvalidMessage> {
    let invalid = |field| InvalidMessage { field };
    Ok(BrowserTab {
        id: NonZeroU32::new(message.tab_id)
            .map(TabId::new)
            .ok_or(invalid("tabs.tab_id"))?,
        address: message.address,
        title: message.title,
        loading: message.loading,
        history: TabHistory {
            can_go_back: message.can_go_back,
            can_go_forward: message.can_go_forward,
        },
        audible: message.audible,
        failure: message
            .failure
            .map(|failure| page_failure(failure).ok_or(invalid("tabs.failure")))
            .transpose()?,
    })
}

fn page_failure_message(failure: PageFailure) -> pb::browser::PageFailure {
    match failure {
        PageFailure::Crashed => pb::browser::PageFailure::Crashed,
        PageFailure::LoadFailed(error) => match error {
            PageLoadError::NameNotResolved => pb::browser::PageFailure::NameNotResolved,
            PageLoadError::ConnectionFailed => pb::browser::PageFailure::ConnectionFailed,
            PageLoadError::Offline => pb::browser::PageFailure::Offline,
            PageLoadError::InvalidCertificate => pb::browser::PageFailure::InvalidCertificate,
            PageLoadError::Blocked => pb::browser::PageFailure::Blocked,
            PageLoadError::Other => pb::browser::PageFailure::LoadFailed,
        },
    }
}

fn page_failure(failure: i32) -> Option<PageFailure> {
    let load_failed = PageFailure::LoadFailed;
    Some(match pb::browser::PageFailure::try_from(failure).ok()? {
        pb::browser::PageFailure::Unspecified => return None,
        pb::browser::PageFailure::NameNotResolved => load_failed(PageLoadError::NameNotResolved),
        pb::browser::PageFailure::ConnectionFailed => load_failed(PageLoadError::ConnectionFailed),
        pb::browser::PageFailure::Offline => load_failed(PageLoadError::Offline),
        pb::browser::PageFailure::InvalidCertificate => {
            load_failed(PageLoadError::InvalidCertificate)
        }
        pb::browser::PageFailure::Blocked => load_failed(PageLoadError::Blocked),
        pb::browser::PageFailure::LoadFailed => load_failed(PageLoadError::Other),
        pb::browser::PageFailure::Crashed => PageFailure::Crashed,
    })
}

const POINTER_SHAPES: [(PointerShape, pb::browser::PointerShape); 16] = [
    (PointerShape::Default, pb::browser::PointerShape::Default),
    (PointerShape::Hand, pb::browser::PointerShape::Hand),
    (PointerShape::Text, pb::browser::PointerShape::Text),
    (PointerShape::Wait, pb::browser::PointerShape::Wait),
    (PointerShape::Progress, pb::browser::PointerShape::Progress),
    (
        PointerShape::Crosshair,
        pb::browser::PointerShape::Crosshair,
    ),
    (PointerShape::Move, pb::browser::PointerShape::Move),
    (
        PointerShape::NotAllowed,
        pb::browser::PointerShape::NotAllowed,
    ),
    (PointerShape::Grab, pb::browser::PointerShape::Grab),
    (PointerShape::Grabbing, pb::browser::PointerShape::Grabbing),
    (PointerShape::Help, pb::browser::PointerShape::Help),
    (
        PointerShape::ColumnResize,
        pb::browser::PointerShape::ColumnResize,
    ),
    (
        PointerShape::RowResize,
        pb::browser::PointerShape::RowResize,
    ),
    (
        PointerShape::NeswResize,
        pb::browser::PointerShape::NeswResize,
    ),
    (
        PointerShape::NwseResize,
        pb::browser::PointerShape::NwseResize,
    ),
    (PointerShape::None, pb::browser::PointerShape::None),
];

fn pointer_shape_message(shape: PointerShape) -> pb::browser::PointerShape {
    POINTER_SHAPES
        .iter()
        .find(|(domain, _)| *domain == shape)
        .map_or(pb::browser::PointerShape::Default, |(_, message)| *message)
}

fn pointer_shape(shape: i32) -> Option<PointerShape> {
    let message = pb::browser::PointerShape::try_from(shape).ok()?;
    POINTER_SHAPES
        .iter()
        .find(|(_, candidate)| *candidate == message)
        .map(|(domain, _)| *domain)
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::*;

    fn tab(id: u32) -> BrowserTab {
        BrowserTab {
            id: TabId::new(NonZeroU32::new(id).unwrap()),
            address: "https://ambience.example/forest".to_owned(),
            title: "Forest".to_owned(),
            loading: true,
            history: TabHistory {
                can_go_back: true,
                can_go_forward: false,
            },
            audible: true,
            failure: Some(PageFailure::LoadFailed(PageLoadError::Offline)),
        }
    }

    #[test]
    fn tabs_survive_the_round_trip() {
        let tabs = BrowserTabs {
            tabs: vec![tab(3), tab(7)],
            selected: Some(TabId::new(NonZeroU32::new(7).unwrap())),
        };

        assert_eq!(browser_tabs(watch_browser_response(&tabs)).unwrap(), tabs);
    }

    #[test]
    fn a_selection_outside_the_tabs_is_invalid() {
        let mut response = watch_browser_response(&BrowserTabs {
            tabs: vec![tab(3)],
            selected: None,
        });
        response.selected_tab_id = Some(4);

        assert_eq!(browser_tabs(response).unwrap_err().field, "selected_tab_id");
    }

    #[test]
    fn view_input_survives_the_round_trip() {
        let inputs = [
            ViewInput::Resize(
                ViewSize::try_new(
                    NonZeroU32::new(640).unwrap(),
                    NonZeroU32::new(360).unwrap(),
                    ScaleFactor::try_new(1.5).unwrap(),
                )
                .unwrap(),
            ),
            ViewInput::PointerButton {
                position: ViewPoint { x: -4, y: 12 },
                button: MouseButton::Right,
                pressed: true,
                click_count: 2,
                modifiers: KeyModifiers {
                    shift: true,
                    ..KeyModifiers::default()
                },
            },
            ViewInput::Key(KeyInput {
                key_code: 0x41,
                text: "á".to_owned(),
                pressed: true,
                repeat: false,
                modifiers: KeyModifiers::default(),
            }),
            ViewInput::InsertText("pasted".to_owned()),
        ];

        for input in inputs {
            assert_eq!(
                view_browser_request(view_browser_message(&input)).unwrap(),
                input
            );
        }
    }

    #[test]
    fn oversized_views_and_local_addresses_are_invalid_arguments() {
        let huge = view_browser_request(pb::browser::ViewBrowserRequest {
            input: Some(pb::browser::view_browser_request::Input::Size(
                pb::browser::ViewSize {
                    width: 6000,
                    height: 100,
                    scale_factor: 1.0,
                },
            )),
        });
        let local_file = navigate_tab_request(&pb::browser::NavigateTabRequest {
            tab_id: 1,
            address: "file:///home/player/notes.html".to_owned(),
        });

        assert_eq!(huge.unwrap_err().code(), Code::InvalidArgument);
        assert_eq!(local_file.unwrap_err().code(), Code::InvalidArgument);
    }

    #[test]
    fn frames_with_the_wrong_pixel_count_are_invalid() {
        let response = pb::browser::ViewBrowserResponse {
            output: Some(pb::browser::view_browser_response::Output::Frame(
                pb::browser::ViewFrame {
                    width: 2,
                    height: 2,
                    x: 0,
                    y: 0,
                    rectangle_width: 2,
                    rectangle_height: 2,
                    pixels: vec![0; 15],
                },
            )),
        };

        assert_eq!(view_output(response).unwrap_err().field, "frame");
    }

    #[test]
    fn every_pointer_shape_survives_the_round_trip() {
        for (shape, _) in POINTER_SHAPES {
            assert_eq!(
                pointer_shape(pointer_shape_message(shape).into()),
                Some(shape)
            );
        }
    }
}
