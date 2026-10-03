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
        EventLoopProxy, Platform, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType},
    },
};

use crate::{backgrounds, ui::AppWindow};

pub(super) const WIDTH: u32 = 1000;
pub(super) const HEIGHT: u32 = 700;
pub(super) const STEP: Duration = Duration::from_nanos(super::FRAME_STEP_NS);
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
    size: PhysicalSize,
}

pub(super) struct Frame {
    pub(super) update_ns: u64,
    pub(super) render_ns: u64,
    pub(super) dirty_pixels: u64,
}

impl Renderer {
    pub(super) fn new(configuration: &super::Configuration) -> anyhow::Result<Self> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
        let clock = Rc::new(Cell::new(Duration::ZERO));
        let (sender, events) = mpsc::channel();
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
            clock: clock.clone(),
            sender,
        }))?;
        let size = PhysicalSize::new(configuration.width, configuration.height);
        window
            .window()
            .dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: f32::from(u16::try_from(configuration.scale_percent)?) / 100.0,
            });
        window.set_size(size);
        Ok(Self {
            window,
            clock,
            events,
            buffers: std::array::from_fn(|_| {
                vec![PremultipliedRgbaColor::default(); (size.width * size.height) as usize]
            }),
            buffer_next: 0,
            size,
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
            let region = renderer.render(buffer, self.size.width as usize);
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
        // Simulated animation frames take longer to rasterize on larger framebuffers.
        let pixels = u64::from(self.size.width) * u64::from(self.size.height);
        let timeout = Duration::from_secs(5 * pixels.div_ceil(3840 * 2160).max(1));
        let deadline = Instant::now() + timeout;
        let mut quiet = 0;
        while quiet < 4 {
            anyhow::ensure!(
                Instant::now() < deadline,
                "GUI did not settle within {} s",
                timeout.as_secs()
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

    pub(super) fn screenshot(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        let mut reference = vec![PremultipliedRgbaColor::default(); self.buffers[0].len()];
        self.window.window().request_redraw();
        self.window.draw_if_needed(|renderer| {
            renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            renderer.render(&mut reference, self.size.width as usize);
            renderer.set_repaint_buffer_type(RepaintBufferType::SwappedBuffers);
        });
        let delta_max = self.buffers[self.buffer_next ^ 1]
            .iter()
            .zip(&reference)
            .flat_map(|(partial, full)| {
                [
                    partial.red.abs_diff(full.red),
                    partial.green.abs_diff(full.green),
                    partial.blue.abs_diff(full.blue),
                ]
            })
            .max()
            .unwrap_or(0);
        let pixels = self.buffers[self.buffer_next ^ 1]
            .iter()
            .flat_map(|pixel| [pixel.red, pixel.green, pixel.blue])
            .collect();
        let image = image::RgbImage::from_raw(self.size.width, self.size.height, pixels)
            .context("invalid screenshot buffer")?;
        image.save(path)?;
        let pixels = reference
            .iter()
            .flat_map(|pixel| [pixel.red, pixel.green, pixel.blue])
            .collect();
        let full = image::RgbImage::from_raw(self.size.width, self.size.height, pixels)
            .context("invalid full screenshot buffer")?;
        full.save(path.with_extension("full.png"))?;
        let pixel_differences = image
            .pixels()
            .zip(full.pixels())
            .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 2))
            .count();
        if pixel_differences > 0 {
            println!(
                "{}: {pixel_differences} pixels differ from a full redraw; channel delta {delta_max}",
                path.display()
            );
        }
        // The reference render changed the renderer's swap history; reseed both buffers.
        for buffer in &mut self.buffers {
            buffer.copy_from_slice(&reference);
        }
        Ok(())
    }

    pub(super) fn show(&self, window: &AppWindow) -> anyhow::Result<()> {
        window.window().set_size(self.size);
        window.show()?;
        Ok(())
    }
}
