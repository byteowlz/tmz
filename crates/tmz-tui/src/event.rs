//! Event handling: terminal events + background task messages.

use crossterm::event::{self, Event as CEvent, KeyEvent};
use std::sync::mpsc;
use std::time::Duration;

/// Events the TUI reacts to.
pub enum Event {
    /// A terminal key press.
    Key(KeyEvent),
    /// Terminal resize.
    Resize,
    /// Periodic tick for background updates.
    Tick,
}

/// Spawns a thread that reads crossterm events and sends them through a channel.
pub fn spawn_event_reader(tick_rate: Duration) -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || loop {
        if event::poll(tick_rate).unwrap_or(false) {
            match event::read() {
                Ok(CEvent::Key(key)) => {
                    if tx.send(Event::Key(key)).is_err() {
                        return;
                    }
                }
                Ok(CEvent::Resize(_, _)) => {
                    if tx.send(Event::Resize).is_err() {
                        return;
                    }
                }
                _ => {}
            }
        }
        // Always send a tick so the UI can update background state
        if tx.send(Event::Tick).is_err() {
            return;
        }
    });

    rx
}
