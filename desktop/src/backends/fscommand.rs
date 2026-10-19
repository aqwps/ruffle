use crate::custom_event::RuffleEvent;

use ruffle_core::external::FsCommandProvider;
use winit::event_loop::EventLoopProxy;

pub struct DesktopFSCommandProvider {
    pub event_loop: EventLoopProxy<RuffleEvent>,
}

impl FsCommandProvider for DesktopFSCommandProvider {
    fn on_fs_command(&self, command: &str, args: &str) -> bool {
        match command {
            "quit" => {
                let _ = self.event_loop.send_event(RuffleEvent::ExitRequested);
            }
            "resize" => {
                let mut parts = args.split('x');
                let width = parts.next().unwrap().parse::<u32>().unwrap();
                let height = parts.next().unwrap().parse::<u32>().unwrap();
                let _ = self
                    .event_loop
                    .send_event(RuffleEvent::Resize(width, height));
            }
            "fullscreen" => {
                match args {
                    "true" => {
                        let _ = self.event_loop.send_event(RuffleEvent::EnterFullScreen);
                    }
                    "false" => {
                        let _ = self.event_loop.send_event(RuffleEvent::ExitFullScreen);
                    }
                    _ => {}
                };
            }
            _ => return false,
        };

        true
    }
}
