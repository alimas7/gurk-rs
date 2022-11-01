//! Draw the UI

use std::fmt;

use chrono::Datelike;
use itertools::Itertools;
use tui::backend::Backend;
use tui::layout::{Constraint, Corner, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::text::{Span, Spans, Text};
use tui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use uuid::Uuid;

use crate::app::App;
use crate::cursor::Cursor;
use crate::data::Message;
use crate::receipt::{Receipt, ReceiptEvent};
use crate::shortcuts::{ShortCut, SHORTCUTS};
use crate::util::utc_timestamp_msec_to_local;

use super::name_resolver::NameResolver;
use super::CHANNEL_VIEW_RATIO;

/// The main function drawing the UI for each frame
pub fn draw<B: Backend>(f: &mut Frame<B>, app: &mut App) {
    if app.is_help() {
        // Display shortcut panel
        let chunks = Layout::default()
            .constraints([
                Constraint::Percentage(15),
                Constraint::Percentage(70),
                Constraint::Percentage(15),
            ])
            .direction(Direction::Horizontal)
            .split(f.size());
        draw_help(f, app, chunks[1]);
        return;
    }
    let chunks = Layout::default()
        .constraints(
            [
                Constraint::Ratio(1, CHANNEL_VIEW_RATIO),
                Constraint::Ratio(3, CHANNEL_VIEW_RATIO),
            ]
            .as_ref(),
        )
        .direction(Direction::Horizontal)
        .split(f.size());

    draw_channels_column(f, app, chunks[0]);
    draw_chat(f, app, chunks[1]);
}

fn draw_channels_column<B: Backend>(f: &mut Frame<B>, app: &mut App, area: Rect) {
    let text_width = area.width.saturating_sub(2) as usize;
    let (wrapped_input, cursor, num_input_lines) = wrap(
        &app.search_box.data,
        app.search_box.cursor.clone(),
        text_width,
    );

    let chunks = Layout::default()
        .constraints(
            [
                Constraint::Length(num_input_lines as u16 + 2),
                Constraint::Min(0),
            ]
            .as_ref(),
        )
        .direction(Direction::Vertical)
        .split(area);

    draw_channels(f, app, chunks[1]);

    let input = Paragraph::new(Text::from(wrapped_input))
        .block(Block::default().borders(Borders::ALL).title("Search"));
    f.render_widget(input, chunks[0]);
    if app.is_searching {
        f.set_cursor(
            chunks[0].x + cursor.col as u16 + 1,
            chunks[0].y + cursor.line as u16 + 1,
        );
    }
}

fn draw_channels<B: Backend>(f: &mut Frame<B>, app: &mut App, area: Rect) {
    let channel_list_width = area.width.saturating_sub(2) as usize;
    let pattern = app.search_box.data.clone();
    app.channel_text_width = channel_list_width;
    app.filter_channels(&pattern);

    let channels: Vec<ListItem> = app
        .data
        .channels
        .iter()
        .map(|channel| {
            let unread_messages_label = if channel.unread_messages != 0 {
                format!(" ({})", channel.unread_messages)
            } else {
                String::new()
            };
            let label = format!("{}{}", app.channel_name(channel), unread_messages_label);
            let label_width = label.width();
            let label = if label.width() <= channel_list_width || unread_messages_label.is_empty() {
                label
            } else {
                let diff = label_width - channel_list_width;
                let mut end = channel.name.width().saturating_sub(diff);
                while !channel.name.is_char_boundary(end) {
                    end += 1;
                }
                format!("{}{}", &channel.name[0..end], unread_messages_label)
            };
            ListItem::new(vec![Spans::from(Span::raw(label))])
        })
        .collect();
    let channels = List::new(channels)
        .block(Block::default().borders(Borders::ALL).title("Channels"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Gray));
    f.render_stateful_widget(channels, area, &mut app.data.channels.state);
}

fn wrap(text: &str, mut cursor: Cursor, width: usize) -> (String, Cursor, usize) {
    let mut res = String::new();

    let mut line = 0;
    let mut col = 0;

    for c in text.chars() {
        // current line too long => wrap
        if col > 0 && col % width == 0 {
            res.push('\n');

            // adjust cursor
            if line < cursor.line {
                cursor.line += 1;
                cursor.idx += 1;
            } else if line == cursor.line && col <= cursor.col {
                cursor.line += 1;
                cursor.col -= col;
                cursor.idx += 1;
            }

            line += 1;
            col = 0;
        }

        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += c.width().unwrap_or(0);
        }
        res.push(c);
    }

    // special case: cursor is at the end of the text and overflows `width`
    if cursor.idx == res.len() && cursor.col == width {
        res.push('\n');
        cursor.line += 1;
        cursor.col = 0;
        cursor.idx += 1;
        line += 1;
    }

    (res, cursor, line + 1)
}

fn draw_chat<B: Backend>(f: &mut Frame<B>, app: &mut App, area: Rect) {
    let text_width = area.width.saturating_sub(2) as usize;
    let (wrapped_input, cursor, num_input_lines) =
        wrap(&app.input.data, app.input.cursor.clone(), text_width);

    let chunks = Layout::default()
        .constraints(
            [
                Constraint::Min(0),
                Constraint::Length(num_input_lines as u16 + 2),
            ]
            .as_ref(),
        )
        .direction(Direction::Vertical)
        .split(area);

    draw_messages(f, app, chunks[0]);

    let title = if app.is_multiline_input {
        "Input (Multiline)"
    } else {
        "Input"
    };

    let input = Paragraph::new(Text::from(wrapped_input))
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(input, chunks[1]);
    if !app.is_searching {
        f.set_cursor(
            chunks[1].x + cursor.col as u16 + 1,  // +1 for frame
            chunks[1].y + cursor.line as u16 + 1, // +1 for frame
        );
    }
}

fn prepare_receipts(app: &mut App, height: usize) {
    let mut to_send = Vec::new();
    let user_id = app.user_id;
    let channel = app
        .data
        .channels
        .state
        .selected()
        .and_then(|idx| app.data.channels.items.get_mut(idx));
    let channel = match channel {
        Some(c) if !c.messages.items.is_empty() => c,
        _ => return,
    };

    let offset = if let Some(selected) = channel.messages.state.selected() {
        channel
            .messages
            .rendered
            .offset
            .min(selected)
            .max(selected.saturating_sub(height))
    } else {
        channel.messages.rendered.offset
    };

    let messages = &mut channel.messages.items[..];

    for msg in messages.iter_mut().rev().skip(offset) {
        if let Receipt::Delivered = msg.receipt {
            if msg.from_id != user_id {
                to_send.push((msg.from_id, msg.arrived_at));
                msg.receipt = Receipt::Read
            }
        }
    }
    if !to_send.is_empty() {
        to_send
            .into_iter()
            .for_each(|(u, t)| app.add_receipt_event(ReceiptEvent::new(u, t, Receipt::Read)))
    }
}

fn draw_messages<B: Backend>(f: &mut Frame<B>, app: &mut App, area: Rect) {
    // area without borders
    let height = area.height.saturating_sub(2) as usize;
    if height == 0 {
        return;
    }
    let width = area.width.saturating_sub(2) as usize;

    prepare_receipts(app, height);

    let channel = app.data.channels.state.selected().and_then(|idx| {
        app.data
            .channels
            .items
            .get(*app.data.channels.filtered_items.get(idx).unwrap())
    });
    let channel = match channel {
        Some(c) if !c.messages.items.is_empty() => c,
        _ => return,
    };

    let writing_people = app.writing_people(channel);

    // Calculate the offset in messages we start rendering with.
    // `offset` includes the selected message (if any), and is at most height-many messages to
    // the selected message, since we can't render more than height-many of them.
    let offset = if let Some(selected) = channel.messages.state.selected() {
        channel
            .messages
            .rendered
            .offset
            .min(selected)
            .max(selected.saturating_sub(height))
    } else {
        channel.messages.rendered.offset
    };

    let messages = &channel.messages.items[..];

    let names = NameResolver::compute_for_channel(app, channel);
    let max_username_width = names.max_name_width();

    // message display options
    const TIME_WIDTH: usize = 6; // width of "00:00 "
    const DELIMITER_WIDTH: usize = 2;
    let mut prefix_width = TIME_WIDTH + max_username_width + DELIMITER_WIDTH;
    if app.config.show_receipts {
        prefix_width += RECEIPT_WIDTH;
    }
    let prefix = " ".repeat(prefix_width);

    // The day of the message at the bottom of the viewport
    let mut previous_msg_day = utc_timestamp_msec_to_local(
        messages
            .iter()
            .rev()
            .skip(offset)
            .map(|msg| msg.arrived_at)
            .next()
            .unwrap_or_default(),
    )
    .num_days_from_ce();

    let messages_from_offset = messages
        .iter()
        .rev()
        .skip(offset)
        .flat_map(|msg| {
            let date_division = display_date_line(msg.arrived_at, &mut previous_msg_day, width);
            let show_receipt = ShowReceipt::from_msg(msg, app.user_id, app.config.show_receipts);
            let msg = display_message(&names, msg, &prefix, width as usize, height, show_receipt);

            [date_division, msg]
        })
        .flatten();

    // counters to accumulate messages as long they fit into the list height,
    // or up to the selected message
    let mut items_height = 0;
    let selected = channel.messages.state.selected().unwrap_or(0);

    let mut items: Vec<ListItem<'static>> = messages_from_offset
        .enumerate()
        .take_while(|(idx, item)| {
            items_height += item.height();
            items_height <= height || offset + *idx <= selected
        })
        .map(|(_, item)| item)
        .collect();

    // calculate the new offset by counting the messages down:
    // we known that we either stopped at the last fitting message or at the selected message
    let mut items_height = height;
    let mut first_idx = 0;
    for (idx, item) in items.iter().enumerate().rev() {
        if item.height() <= items_height {
            items_height -= item.height();
            first_idx = idx;
        } else {
            break;
        }
    }
    let offset = offset + first_idx;
    items = items.split_off(first_idx);

    // add unread messages line
    let unread_messages = channel.unread_messages;
    if unread_messages > 0 && unread_messages < items.len() {
        let new_message_line = "-".repeat(prefix_width)
            + "new messages"
            + &"-".repeat(width.saturating_sub(prefix_width));
        items.insert(unread_messages, ListItem::new(Span::from(new_message_line)));
    }

    let title: String = if let Some(writing_people) = writing_people {
        format!("Messages {}", writing_people)
    } else {
        "Messages".to_string()
    };

    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Gray))
        .start_corner(Corner::BottomLeft);

    // re-borrow channel mutably
    let channel_idx = app.data.channels.state.selected().unwrap_or_default();
    let channel = &mut app.data.channels.items[channel_idx];

    // update selected state to point within `items`
    let state = &mut channel.messages.state;
    let selected_global = state.selected();
    if let Some(selected) = selected_global {
        state.select(Some(selected - offset));
    }

    f.render_stateful_widget(list, area, state);

    // restore selected state and update offset
    state.select(selected_global);
    channel.messages.rendered.offset = offset;
}

fn display_time(timestamp: u64) -> String {
    utc_timestamp_msec_to_local(timestamp)
        .format("%R ")
        .to_string()
}

const RECEIPT_WIDTH: usize = 2;

/// Ternary state whether to show receipt for a message
enum ShowReceipt {
    // show receipt for this message
    Yes,
    // don't show receipt for this message
    No,
    // don't show receipt for any message
    Never,
}

impl ShowReceipt {
    fn from_msg(msg: &Message, user_id: Uuid, config_show_receipts: bool) -> Self {
        if config_show_receipts {
            if user_id == msg.from_id {
                Self::Yes
            } else {
                Self::No
            }
        } else {
            Self::Never
        }
    }
}

fn display_receipt(receipt: Receipt, show: ShowReceipt) -> &'static str {
    use ShowReceipt::*;
    match (show, receipt) {
        (Yes, Receipt::Nothing) => "  ",
        (Yes, Receipt::Sent) => "○ ",
        (Yes, Receipt::Delivered) => "◉ ",
        (Yes, Receipt::Read) => "● ",
        (No, _) => "  ",
        (Never, _) => "",
    }
}

#[allow(clippy::too_many_arguments)]
fn display_message(
    names: &NameResolver,
    msg: &Message,
    prefix: &str,
    width: usize,
    height: usize,
    show_receipt: ShowReceipt,
) -> Option<ListItem<'static>> {
    let receipt = Span::styled(
        display_receipt(msg.receipt, show_receipt),
        Style::default().fg(Color::Yellow),
    );

    let time = Span::styled(
        display_time(msg.arrived_at),
        Style::default().fg(Color::Yellow),
    );

    let (from, from_color) = names.resolve(msg.from_id);

    let from = Span::styled(
        textwrap::indent(
            &from,
            &" ".repeat(
                names
                    .max_name_width()
                    .checked_sub(from.width())
                    .unwrap_or_default(),
            ),
        ),
        Style::default().fg(from_color),
    );
    let delimiter = Span::from(": ");

    let wrap_opts = textwrap::Options::new(width)
        .initial_indent(prefix)
        .subsequent_indent(prefix);

    // collect message text
    let mut text = msg.message.clone().unwrap_or_default();
    add_attachments(msg, &mut text);
    if text.is_empty() {
        return None; // no text => nothing to render
    }
    add_reactions(msg, &mut text);

    let mut spans: Vec<Spans> = vec![];

    // prepend quote if any
    let quote_text = msg
        .quote
        .as_ref()
        .and_then(|quote| displayed_quote(names, quote));
    if let Some(quote_text) = quote_text.as_ref() {
        let quote_prefix = format!("{}> ", prefix);
        let quote_wrap_opts = textwrap::Options::new(width.saturating_sub(2))
            .initial_indent(&quote_prefix)
            .subsequent_indent(&quote_prefix);
        let quote_style = Style::default().fg(Color::Rgb(150, 150, 150));
        spans = textwrap::wrap(quote_text, quote_wrap_opts)
            .into_iter()
            .enumerate()
            .map(|(idx, line)| {
                let res = if idx == 0 {
                    vec![
                        receipt.clone(),
                        time.clone(),
                        from.clone(),
                        delimiter.clone(),
                        Span::styled(line.strip_prefix(prefix).unwrap().to_string(), quote_style),
                    ]
                } else {
                    vec![Span::styled(line.to_string(), quote_style)]
                };
                Spans::from(res)
            })
            .collect();
    }

    let add_time = spans.is_empty();
    spans.extend(
        textwrap::wrap(&text, &wrap_opts)
            .into_iter()
            .enumerate()
            .map(|(idx, line)| {
                let res = if add_time && idx == 0 {
                    vec![
                        receipt.clone(),
                        time.clone(),
                        from.clone(),
                        delimiter.clone(),
                        Span::from(line.strip_prefix(prefix).unwrap().to_string()),
                    ]
                } else {
                    vec![Span::from(line.to_string())]
                };
                Spans::from(res)
            }),
    );

    if spans.len() > height {
        // span is too big to be shown fully
        spans.resize(height - 1, Spans::from(""));
        spans.push(Spans::from(format!("{}[...]", prefix)));
    }
    Some(ListItem::new(Text::from(spans)))
}

fn display_date_line(
    msg_timestamp: u64,
    previous_msg_day: &mut i32,
    width: usize,
) -> Option<ListItem<'static>> {
    let local_time = utc_timestamp_msec_to_local(msg_timestamp);
    let current_msg_day = local_time.num_days_from_ce();

    if current_msg_day != *previous_msg_day {
        *previous_msg_day = current_msg_day;

        // Weekday and locale's date representation (e.g., 12/31/99)
        let date = format!("{:=^width$}", local_time.format(" %A, %x "));
        Some(ListItem::new(Span::from(date)))
    } else {
        None
    }
}

fn add_attachments(msg: &Message, out: &mut String) {
    if !msg.attachments.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }

        fmt::write(
            out,
            format_args!(
                "{}",
                msg.attachments
                    .iter()
                    .format_with("\n", |attachment, f| f(&format_args!(
                        "<file://{}>",
                        attachment.filename.display()
                    )))
            ),
        )
        .expect("formatting attachments failed");
    }
}

fn add_reactions(msg: &Message, out: &mut dyn fmt::Write) {
    if !msg.reactions.is_empty() {
        fmt::write(
            out,
            format_args!(
                " [{}]",
                msg.reactions.iter().map(|(_, emoji)| emoji).format("")
            ),
        )
        .expect("formatting reactions failed");
    }
}

fn draw_help<B: Backend>(f: &mut Frame<B>, app: &mut App, area: Rect) {
    let max_event_width = SHORTCUTS
        .iter()
        .map(
            |ShortCut {
                 event: ev,
                 description: _,
             }| { (**ev).width() },
        )
        .max()
        .unwrap_or(0);

    let width = area.width.saturating_sub(2) as usize;
    let delimiter = Span::from(": ");
    const DELIMITER_WIDTH: usize = 2;
    let prefix_width = max_event_width + DELIMITER_WIDTH + 1; // +1 because we add 1 extra " " between the event and the delimiter
    let prefix = " ".repeat(prefix_width);

    let wrap_opts = textwrap::Options::new(width)
        .initial_indent(&prefix)
        .subsequent_indent(&prefix);

    let shorts: Vec<ListItem> = SHORTCUTS
        .iter()
        .map(
            |ShortCut {
                 event: ev,
                 description: desc,
             }| {
                let event = ev;
                let description = desc;

                let wrapped = textwrap::wrap(description, &wrap_opts);

                let mut res = Vec::new();

                wrapped.into_iter().enumerate().for_each(|(i, line)| {
                    let mut truc = Vec::new();
                    if i == 0 {
                        let event_span = Span::from(textwrap::indent(
                            &" ".repeat(
                                max_event_width
                                    .checked_sub(event.width())
                                    .unwrap_or_default()
                                    + 1,
                            ),
                            event,
                        ));
                        truc.push(event_span);
                        truc.push(delimiter.clone());
                        truc.push(Span::from(line.strip_prefix(&prefix).unwrap().to_string()));
                    } else {
                        truc.push(Span::from(line.to_string()))
                    };

                    let spans = Spans::from(truc);
                    res.push(spans);
                });

                ListItem::new(Text::from(res))
            },
        )
        .collect();

    let shorts_widget =
        List::new(shorts).block(Block::default().borders(Borders::ALL).title("Help"));
    f.render_stateful_widget(shorts_widget, area, &mut app.data.channels.state);
}

fn displayed_quote(names: &NameResolver, quote: &Message) -> Option<String> {
    let (name, _) = names.resolve(quote.from_id);
    Some(format!("({}) {}", name, quote.message.as_ref()?))
}

#[cfg(test)]
mod tests {
    use crate::signal::Attachment;

    use super::*;

    const USER_ID: Uuid = Uuid::nil();

    // formatting test options
    const PREFIX: &str = "                  ";
    const WIDTH: usize = 60;
    const HEIGHT: usize = 10;

    fn test_attachment() -> Attachment {
        Attachment {
            id: "2022-01-16T11:59:58.405665+00:00".to_string(),
            content_type: "image/jpeg".into(),
            filename: "/tmp/gurk/signal-2022-01-16T11:59:58.405665+00:00.jpg".into(),
            size: 238987,
        }
    }

    fn name_resolver() -> NameResolver<'static> {
        NameResolver::single_user(USER_ID, "boxdot".to_string(), Color::Green)
    }

    fn test_message() -> Message {
        Message {
            from_id: USER_ID,
            message: None,
            arrived_at: 1642334397421,
            quote: None,
            attachments: vec![],
            reactions: vec![],
            receipt: Receipt::Sent,
        }
    }

    #[test]
    fn test_display_attachment_only_message() {
        let names = name_resolver();
        let msg = Message {
            attachments: vec![test_attachment()],
            ..test_message()
        };
        let rendered = display_message(&names, &msg, PREFIX, WIDTH, HEIGHT, ShowReceipt::Never);

        let expected = ListItem::new(Text::from(vec![
            Spans(vec![
                Span::styled("", Style::default().fg(Color::Yellow)),
                Span::styled(
                    display_time(msg.arrived_at),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("boxdot", Style::default().fg(Color::Green)),
                Span::raw(": "),
                Span::raw("<file:///tmp/gurk/signal-2022-01-"),
            ]),
            Spans(vec![Span::raw(
                "                  16T11:59:58.405665+00:00.jpg>",
            )]),
        ]));
        assert_eq!(rendered, Some(expected));
    }

    #[test]
    fn test_display_text_and_attachment_message() {
        let names = name_resolver();
        let msg = Message {
            message: Some("Hello, World!".into()),
            attachments: vec![test_attachment()],
            ..test_message()
        };
        let rendered = display_message(&names, &msg, PREFIX, WIDTH, HEIGHT, ShowReceipt::Never);

        let expected = ListItem::new(Text::from(vec![
            Spans(vec![
                Span::styled("", Style::default().fg(Color::Yellow)),
                Span::styled(
                    display_time(msg.arrived_at),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled("boxdot", Style::default().fg(Color::Green)),
                Span::raw(": "),
                Span::raw("Hello, World!"),
            ]),
            Spans(vec![Span::raw(
                "                  <file:///tmp/gurk/signal-2022-01-",
            )]),
            Spans(vec![Span::raw(
                "                  16T11:59:58.405665+00:00.jpg>",
            )]),
        ]));
        assert_eq!(rendered, Some(expected));
    }

    #[test]
    fn test_display_sent_receipt() {
        let names = name_resolver();
        let msg = Message {
            message: Some("Hello, World!".into()),
            receipt: Receipt::Sent,
            ..test_message()
        };
        let show_receipt = ShowReceipt::from_msg(&msg, USER_ID, true);
        let rendered = display_message(&names, &msg, PREFIX, WIDTH, HEIGHT, show_receipt);

        let expected = ListItem::new(Text::from(vec![Spans(vec![
            Span::styled("○ ", Style::default().fg(Color::Yellow)),
            Span::styled(
                display_time(msg.arrived_at),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("boxdot", Style::default().fg(Color::Green)),
            Span::raw(": "),
            Span::raw("Hello, World!"),
        ])]));
        assert_eq!(rendered, Some(expected));
    }

    #[test]
    fn test_display_received_receipt() {
        let names = name_resolver();
        let msg = Message {
            message: Some("Hello, World!".into()),
            receipt: Receipt::Delivered,
            ..test_message()
        };
        let show_receipt = ShowReceipt::from_msg(&msg, USER_ID, true);
        let rendered = display_message(&names, &msg, PREFIX, WIDTH, HEIGHT, show_receipt);

        let expected = ListItem::new(Text::from(vec![Spans(vec![
            Span::styled("◉ ", Style::default().fg(Color::Yellow)),
            Span::styled(
                display_time(msg.arrived_at),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("boxdot", Style::default().fg(Color::Green)),
            Span::raw(": "),
            Span::raw("Hello, World!"),
        ])]));
        assert_eq!(rendered, Some(expected));
    }

    #[test]
    fn test_display_delivered_receipt() {
        let names = name_resolver();
        let msg = Message {
            message: Some("Hello, World!".into()),
            receipt: Receipt::Read,
            ..test_message()
        };
        let show_receipt = ShowReceipt::from_msg(&msg, USER_ID, true);
        let rendered = display_message(&names, &msg, PREFIX, WIDTH, HEIGHT, show_receipt);

        let expected = ListItem::new(Text::from(vec![Spans(vec![
            Span::styled("● ", Style::default().fg(Color::Yellow)),
            Span::styled(
                display_time(msg.arrived_at),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("boxdot", Style::default().fg(Color::Green)),
            Span::raw(": "),
            Span::raw("Hello, World!"),
        ])]));
        assert_eq!(rendered, Some(expected));
    }

    #[test]
    fn test_display_show_receipts_disabled() {
        let names = name_resolver();
        let msg = Message {
            message: Some("Hello, World!".into()),
            receipt: Receipt::Read,
            ..test_message()
        };
        let show_receipt = ShowReceipt::from_msg(&msg, USER_ID, false);
        let rendered = display_message(&names, &msg, PREFIX, WIDTH, HEIGHT, show_receipt);

        let expected = ListItem::new(Text::from(vec![Spans(vec![
            Span::styled("", Style::default().fg(Color::Yellow)),
            Span::styled(
                display_time(msg.arrived_at),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("boxdot", Style::default().fg(Color::Green)),
            Span::raw(": "),
            Span::raw("Hello, World!"),
        ])]));
        assert_eq!(rendered, Some(expected));
    }

    #[test]
    fn test_display_receipts_for_incoming_message() {
        let user_id = Uuid::from_u128(1);
        let names = NameResolver::single_user(user_id, "boxdot".to_string(), Color::Green);
        let msg = Message {
            from_id: user_id,
            message: Some("Hello, World!".into()),
            receipt: Receipt::Read,
            ..test_message()
        };
        let show_receipt = ShowReceipt::from_msg(&msg, USER_ID, true);
        let rendered = display_message(&names, &msg, PREFIX, WIDTH, HEIGHT, show_receipt);

        let expected = ListItem::new(Text::from(vec![Spans(vec![
            Span::styled("  ", Style::default().fg(Color::Yellow)),
            Span::styled(
                display_time(msg.arrived_at),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("boxdot", Style::default().fg(Color::Green)),
            Span::raw(": "),
            Span::raw("Hello, World!"),
        ])]));
        assert_eq!(rendered, Some(expected));
    }
}