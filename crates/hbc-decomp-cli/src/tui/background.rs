//! Background-task polling: drains the diff-worker and pipeline-build channels
//! into `App` state without blocking the UI thread.

use std::sync::mpsc::TryRecvError;
use std::sync::Arc;

use super::app::App;
use super::debug_log;
use super::diff::DiffProgressMsg;

impl App {
    pub fn poll_background_tasks(&mut self) {
        let mut messages = Vec::new();
        let mut disconnected = false;
        const MAX_MESSAGES_PER_TICK: usize = 128;

        if let Some(rx) = self.diff_rx.as_ref() {
            for _ in 0..MAX_MESSAGES_PER_TICK {
                match rx.try_recv() {
                    Ok(msg) => messages.push(msg),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        for msg in messages {
            match msg {
                DiffProgressMsg::Item {
                    name,
                    status,
                    done,
                    total,
                } => {
                    self.diff_status.insert(name.clone(), status);
                    self.diff_progress_done = done;
                    self.diff_progress_total = total;
                    self.add_discovered_name(name);
                }
                DiffProgressMsg::Finished { final_status } => {
                    self.diff_status = final_status;
                    self.diff_analyzing = false;
                    self.diff_progress_done = self.diff_status.len();
                    if self.diff_progress_total == 0 {
                        self.diff_progress_total = self.diff_status.len();
                    }
                    self.all_function_names = self.diff_status.keys().cloned().collect();
                    self.all_function_names.sort();
                    self.known_names = self.all_function_names.iter().cloned().collect();
                    self.update_search();
                    self.diff_rx = None;
                }
            }
        }

        if disconnected {
            self.diff_analyzing = false;
            self.diff_rx = None;
        }

        // Poll pipeline context (file 1)
        if let Some(rx) = self.pipeline_rx.as_ref() {
            match rx.try_recv() {
                Ok(ctx) => {
                    debug_log("[TUI] Pipeline context (file 1) received");
                    self.pipeline_ctx = Some(Arc::new(ctx));
                    self.pipeline_building = false;
                    self.pipeline_rx = None;
                    self.decompile_cache.clear();
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.pipeline_building = false;
                    self.pipeline_rx = None;
                }
            }
        }

        // Poll pipeline context (file 2)
        if let Some(rx) = self.pipeline_rx2.as_ref() {
            match rx.try_recv() {
                Ok(ctx) => {
                    debug_log("[TUI] Pipeline context (file 2) received");
                    self.pipeline_ctx2 = Some(Arc::new(ctx));
                    self.pipeline_building2 = false;
                    self.pipeline_rx2 = None;
                    self.decompile_cache2.clear();
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.pipeline_building2 = false;
                    self.pipeline_rx2 = None;
                }
            }
        }
    }

}
