//! Everything that arrives on the main loop's channel.

use crate::layout::PaneId;
use crate::system::SystemInfo;
use std::sync::mpsc::Sender;

#[derive(Debug)]
pub struct IpcRequest {
    pub cmd: String,
    pub args: Vec<String>,
}

pub enum Msg {
    /// A key/mouse/paste event from the host terminal.
    Input(ratatui::crossterm::event::Event),
    /// Raw bytes read from a pane's pty.
    Output(PaneId, Vec<u8>),
    /// A pane's child process ended.
    Exited(PaneId),
    /// The loop's heartbeat: repaint, advance the clock, blink the cursor.
    Tick,
    /// A fresh system snapshot from the background probe thread.
    Sys(Box<SystemInfo>),
    /// A `tlc` client is waiting for a reply on this channel.
    Ipc(IpcRequest, Sender<String>),
}
