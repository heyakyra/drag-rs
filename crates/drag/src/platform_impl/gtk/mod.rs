// Copyright 2023-2023 CrabNebula Ltd.
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use crate::{CursorPosition, DragItem, DragMode, DragResult, Error, Image, Options};
use gdkx11::{
    gdk,
    glib::{ObjectExt, Propagation, SignalHandlerId},
};
use gtk::{
    gdk_pixbuf,
    prelude::{
        DeviceExt, DragContextExtManual, PixbufLoaderExt, SeatExt, WidgetExt, WidgetExtManual,
    },
};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, Mutex},
};

pub fn start_drag<F: Fn(DragResult, CursorPosition) + Send + 'static>(
    window: &gtk::ApplicationWindow,
    item: DragItem,
    image: Image,
    on_drop_callback: F,
    options: Options,
) -> crate::Result<()> {
    log::debug!("Starting drag operation with mode: {:?}", options.mode);
    let handler_ids: Arc<Mutex<Vec<SignalHandlerId>>> = Arc::new(Mutex::new(vec![]));
    let drag_action = match options.mode {
        DragMode::Copy => gdk::DragAction::COPY,
        DragMode::Move => gdk::DragAction::MOVE,
    };

    log::debug!("Setting drag source with action: {:?}", drag_action);
    window.drag_source_set(gdk::ModifierType::BUTTON1_MASK, &[], drag_action);

    match item {
        DragItem::Files(paths) => {
            log::debug!("Setting up file drag with {} paths", paths.len());
            window.drag_source_add_uri_targets();
            handler_ids
                .lock()
                .unwrap()
                .push(window.connect_drag_data_get(move |_, _, data, _, _| {
                    log::debug!("Preparing URIs for drag data");
                    // Percent-encode: `set_uris` converts the list to ASCII and
                    // silently sets nothing if any byte is non-ASCII.
                    let uris: Vec<String> = paths
                        .iter()
                        .filter_map(|path| match gtk::glib::filename_to_uri(path, None) {
                            Ok(uri) => Some(uri.to_string()),
                            Err(e) => {
                                log::warn!("Failed to build URI for {}: {}", path.display(), e);
                                None
                            }
                        })
                        .collect();
                    let uris: Vec<&str> = uris.iter().map(|s| s.as_str()).collect();
                    log::debug!("Setting URIs: {:?}", uris);
                    data.set_uris(&uris);
                }));
        }
        DragItem::Data { .. } => {
            // Currently leaving it as is as we can utilize it as a dummy dragging feature
            // on_drop_callback(DragResult::Cancel, get_cursor_position(window).unwrap());
            // return Ok(());
        }
    }

    if let Some(target_list) = &window.drag_source_get_target_list() {
        log::debug!("Got target list, initiating drag");
        if let Some(drag_context) = window.drag_begin_with_coordinates(
            target_list,
            drag_action,
            gdk::ffi::GDK_BUTTON1_MASK as i32,
            None,
            -1,
            -1,
        ) {
            log::debug!("Drag context created successfully");
            // `drop-performed` fires before the target requests the data, so
            // the result is only recorded there. Cleanup and the callback wait
            // for `drag-end`, which GTK emits after the data transfer finished
            // (or the drag was cancelled).
            let result = Rc::new(Cell::new(DragResult::Cancel));
            on_drop_failed(result.clone(), window, &handler_ids, &options);
            on_drop_performed(result.clone(), &drag_context);
            on_drag_end(on_drop_callback, result, window, &handler_ids);

            log::debug!("Setting up drag icon");
            let icon_pixbuf: Option<gdk_pixbuf::Pixbuf> = match &image {
                Image::Raw(data) => image_binary_to_pixbuf(data),
                Image::File(path) => match std::fs::read(path) {
                    Ok(bytes) => image_binary_to_pixbuf(&bytes),
                    Err(_) => None,
                },
            };
            if let Some(icon) = icon_pixbuf {
                drag_context.drag_set_icon_pixbuf(&icon, 0, 0);
            }

            Ok(())
        } else {
            Err(crate::Error::FailedToStartDrag)
        }
    } else {
        Err(crate::Error::EmptyTargetList)
    }
}

fn image_binary_to_pixbuf(data: &[u8]) -> Option<gdk_pixbuf::Pixbuf> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader
        .write(data)
        .and_then(|_| loader.close())
        .map_err(|_| ())
        .and_then(|_| loader.pixbuf().ok_or(()))
        .ok()
}

fn clear_signal_handlers(window: &gtk::ApplicationWindow, handler_ids: &mut Vec<SignalHandlerId>) {
    for handler_id in handler_ids.drain(..) {
        window.disconnect(handler_id);
    }
}

fn on_drop_failed(
    result: Rc<Cell<DragResult>>,
    window: &gtk::ApplicationWindow,
    handler_ids: &Arc<Mutex<Vec<SignalHandlerId>>>,
    options: &Options,
) {
    log::debug!("Setting up drop failed handler");
    let skip_animatation_on_cancel_or_failure = options.skip_animatation_on_cancel_or_failure;

    handler_ids
        .lock()
        .unwrap()
        .push(window.connect_drag_failed(move |_, _, _drag_result| {
            log::debug!("Drag failed or cancelled");
            result.set(DragResult::Cancel);
            if skip_animatation_on_cancel_or_failure {
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        }));
}

fn cleanup_signal_handlers(
    handler_ids: &Arc<Mutex<Vec<SignalHandlerId>>>,
    window: &gtk::ApplicationWindow,
) {
    log::debug!("Cleaning up signal handlers");
    let handler_ids = &mut handler_ids.lock().unwrap();
    clear_signal_handlers(window, handler_ids);
    window.drag_source_unset();
    log::debug!("Signal handlers cleaned up");
}

fn on_drop_performed(result: Rc<Cell<DragResult>>, drag_context: &gdk::DragContext) {
    log::debug!("Setting up drop performed handler");
    drag_context.connect_drop_performed(move |context, _| {
        log::debug!("Drop performed, waiting for drag-end");
        log::trace!("Selected action: {:?}", context.selected_action());
        log::trace!("Suggested action: {:?}", context.suggested_action());
        result.set(DragResult::Dropped);
    });
}

fn on_drag_end<F: Fn(DragResult, CursorPosition) + Send + 'static>(
    callback: F,
    result: Rc<Cell<DragResult>>,
    window: &gtk::ApplicationWindow,
    handler_ids: &Arc<Mutex<Vec<SignalHandlerId>>>,
) {
    log::debug!("Setting up drag end handler");
    let handler_ids_clone = handler_ids.clone();

    handler_ids
        .lock()
        .unwrap()
        .push(window.connect_drag_end(move |window, _| {
            log::debug!("Drag ended with result: {:?}", result.get());
            cleanup_signal_handlers(&handler_ids_clone, window);
            callback(result.get(), get_cursor_position(window).unwrap());
        }));
}

fn get_cursor_position(window: &gtk::ApplicationWindow) -> Result<CursorPosition, Error> {
    if let Some(cursor) = window
        .display()
        .default_seat()
        .and_then(|seat| seat.pointer())
    {
        let (_, x, y) = cursor.position();
        Ok(CursorPosition { x, y })
    } else {
        Err(Error::FailedToGetCursorPosition)
    }
}
