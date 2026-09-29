use std::{
    cell::Cell,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use slint::{
    ComponentHandle as _, EventLoopError, PhysicalSize, PlatformError,
    platform::{
        EventLoopProxy, Platform, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType},
    },
};

use crate::{backgrounds, ui::AppWindow};

pub(super) const WIDTH: u32 = 1000;
pub(super) const HEIGHT: u32 = 700;
pub(super) const STEP: Duration = Duration::from_nanos(8_333_333);
type Event = Box<dyn FnOnce() + Send>;

struct Backend {
    window: Rc<MinimalSoftwareWindow>,
    clock: Rc<Cell<Duration>>,
    sender: mpsc::Sender<Event>,
}

impl Platform for Backend {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.clock.get()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.sender.clone())))
    }
}

struct Proxy(mpsc::Sender<Event>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        Ok(())
    }

    fn invoke_from_event_loop(&self, event: Event) -> Result<(), EventLoopError> {
        self.0
            .send(event)
            .map_err(|_| EventLoopError::EventLoopTerminated)
    }
}

pub(super) struct Renderer {
    window: Rc<MinimalSoftwareWindow>,
    clock: Rc<Cell<Duration>>,
    events: mpsc::Receiver<Event>,
    buffers: [Vec<PremultipliedRgbaColor>; 2],
    buffer_next: usize,
}

pub(super) struct Frame {
    pub(super) update_ns: u64,
    pub(super) render_ns: u64,
    pub(super) dirty_pixels: u64,
}

impl Renderer {
    pub(super) fn new() -> anyhow::Result<Self> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
        let clock = Rc::new(Cell::new(Duration::ZERO));
        let (sender, events) = mpsc::channel();
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
            clock: clock.clone(),
            sender,
        }))?;
        window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
        Ok(Self {
            window,
            clock,
            events,
            buffers: std::array::from_fn(|_| {
                vec![PremultipliedRgbaColor::default(); (WIDTH * HEIGHT) as usize]
            }),
            buffer_next: 0,
        })
    }

    pub(super) fn frame(&mut self) -> anyhow::Result<Frame> {
        let started = Instant::now();
        self.clock.set(self.clock.get() + STEP);
        for event in self.events.try_iter().take(10_000) {
            event();
        }
        slint::platform::update_timers_and_animations();
        let update_ns = u64::try_from(started.elapsed().as_nanos())?;
        let mut render_ns = 0;
        let mut dirty_pixels = 0;
        let buffer = &mut self.buffers[self.buffer_next];
        let rendered = self.window.draw_if_needed(|renderer| {
            let started = Instant::now();
            let region = renderer.render(buffer, WIDTH as usize);
            render_ns = started.elapsed().as_nanos();
            dirty_pixels = region
                .iter()
                .map(|(_, size)| u64::from(size.width) * u64::from(size.height))
                .sum();
        });
        if rendered {
            self.buffer_next ^= 1;
        }
        Ok(Frame {
            update_ns,
            render_ns: u64::try_from(render_ns)?,
            dirty_pixels,
        })
    }

    pub(super) fn settle(&mut self) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut quiet = 0;
        while quiet < 4 {
            anyhow::ensure!(
                Instant::now() < deadline,
                "artwork did not settle within 5 s"
            );
            let frame = self.frame()?;
            quiet = if frame.dirty_pixels == 0 && backgrounds::is_idle() {
                quiet + 1
            } else {
                0
            };
            std::thread::sleep(STEP);
        }
        Ok(())
    }

    pub(super) fn screenshot(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let pixels = self.buffers[self.buffer_next ^ 1]
            .iter()
            .flat_map(|pixel| [pixel.red, pixel.green, pixel.blue])
            .collect();
        let image = image::RgbImage::from_raw(WIDTH, HEIGHT, pixels)
            .context("invalid screenshot buffer")?;
        image.save(path)?;
        Ok(())
    }

    pub(super) fn show(window: &AppWindow) -> anyhow::Result<()> {
        window.window().set_size(PhysicalSize::new(WIDTH, HEIGHT));
        window.show()?;
        Ok(())
    }
}
