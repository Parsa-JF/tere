use std::convert::{From, TryFrom};
use std::io::{Stderr, Write};
use crossterm::{
    execute, queue,
    terminal,
    cursor,
    style::{self, Stylize, Attribute},
    event::{
        read as read_event,
        Event,
        KeyCode,
        KeyModifiers,
    },
    Result as CTResult,
};
use home::home_dir;

use clap::{App, Arg, ArgMatches};

const HEADER_SIZE: u16 = 1;
const INFO_WIN_SIZE: u16 = 1;
const FOOTER_SIZE: u16 = 1;

//TODO: rustfmt
//TODO: clippy

mod app_state;
use app_state::{TereAppState, CustomDirEntry};

#[derive(Debug)]
enum TereError {
    WindowInit(String, i32),
    IoError(std::io::Error),
}

impl From<std::io::Error> for TereError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

/// This struct groups together ncurses windows for the main content, header and
/// footer, and an application state object
struct TereTui<'a> {
    window: &'a Stderr,
    app_state: TereAppState,
}

// Dimensions (width, height) of main window
fn main_window_size() -> CTResult<(u16, u16)> {
    let (w, h) = terminal::size()?;
    Ok((w, h.checked_sub(HEADER_SIZE + INFO_WIN_SIZE + FOOTER_SIZE).unwrap_or(0)))
}

impl<'a> TereTui<'a> {

    pub fn init(args: &ArgMatches, window: &'a mut Stderr) -> Result<Self, TereError> {
        let (w, h) = main_window_size()?;
        let state = TereAppState::init(
            args,
            // TODO: have to convert to u32 here. but correct solution would be to use u16 instead in app_state as well
            w.into(), h.into()
        );
        let mut ret = Self {
            window: window,
            app_state: state,
        };

        ret.update_header();
        ret.redraw_all_windows();
        Ok(ret)
    }

    /// Queue up a command to clear a given row (starting from 0). Must be executed/flushed
    /// separately.
    fn queue_clear_row(&mut self, row: u16) -> CTResult<()> {
        queue!(
            self.window,
            cursor::MoveTo(0, row),
            terminal::Clear(terminal::ClearType::CurrentLine),
        )
    }

    pub fn redraw_header(&mut self) -> CTResult<()> {
        //TODO: what to do if window is narrower than path?
        // add "..." to beginning? or collapse folder names? make configurable?
        // at least, truncate towards the left instead of to the right

        // must use variable here b/c can't borrow 'self' twice in execute!() below
        let mut win = self.window;
        self.queue_clear_row(0);
        execute!(
            win,
            cursor::MoveTo(0, 0),
            style::SetAttribute(Attribute::Reset),
            style::Print(&self.app_state.header_msg.clone().bold().underlined()),
        )?;
        Ok(())
    }

    pub fn update_header(&mut self) {
        self.app_state.update_header();
        // TODO: consider removing redraw here... (is inconsistent with the rest of the 'update' functions)
        self.redraw_header();
    }

    pub fn redraw_info_window(&mut self) -> CTResult<()> {
        let (_, h) = crossterm::terminal::size()?;
        let info_win_row = h - FOOTER_SIZE - INFO_WIN_SIZE;

        self.queue_clear_row(info_win_row);
        let mut win = self.window;
        execute!(
            win,
            cursor::MoveTo(0, info_win_row),
            style::SetAttribute(Attribute::Reset),
            style::Print(&self.app_state.info_msg.clone().bold()),
        )
    }

    /// Set/update the current info message and redraw the info window
    pub fn info_message(&mut self, msg: &str) {
        //TODO: add thread with timeout that will clear the info message after x seconds?
        self.app_state.info_msg = msg.to_string();
        self.redraw_info_window();
    }

    pub fn error_message(&mut self, msg: &str) {
        //TODO: red color (also: make it configurable)
        let mut error_msg = String::from("error: ");
        error_msg.push_str(msg);
        self.info_message(&error_msg);
    }

    pub fn redraw_footer(&mut self) -> CTResult<()> {
        let (w, h) = crossterm::terminal::size()?;
        let footer_win_row = h - FOOTER_SIZE;
        self.queue_clear_row(footer_win_row);

        let mut win = self.window;
        let mut extra_msg = String::new();

        if self.app_state.is_searching() {
            //self.footer_win.mvaddstr(0, 0, &self.app_state.search_string());
            queue!(
                win,
                cursor::MoveTo(0, footer_win_row),
                style::SetAttribute(Attribute::Reset),
                style::Print(&self.app_state.search_string().clone().bold()),
            )?;
            extra_msg.push_str(&format!("{} / {} / {}",
                               self.app_state.search_matches()
                                   .current_pos().map(|i| i + 1).unwrap_or(0),
                               self.app_state.search_matches().len(),
                               self.app_state.ls_output_buf.len()));
        } else {
            //TODO: show no. of files/folders separately? like 'n folders, n files'
            let cursor_idx = self.app_state.cursor_pos +
                             self.app_state.scroll_pos + 1;
            extra_msg.push_str(&format!("{} / {}",
                               cursor_idx,
                               self.app_state.ls_output_buf.len()));
        }
        execute!(
            win,
            cursor::MoveTo(w - extra_msg.len() as u16, footer_win_row),
            style::SetAttribute(Attribute::Reset),
            style::Print(extra_msg.bold()),
        )
    }

    /// Convert a row number (starting from 0 at the top of the main window)
    /// to an index into the ls_output_buf
    fn row_to_buf_idx(&self, row: u16) -> usize {
        (self.app_state.scroll_pos + row as u32) as usize
    }

    /// Get the item from ls_output_buf that should be displayed on row `row` of the main window.
    /// Row 0 is the first row of the main window.
    fn get_item_at_row(&self, row: u16) -> Option<&CustomDirEntry> {
        let idx = self.row_to_buf_idx(row);
        self.app_state.ls_output_buf.get(idx)
    }

    fn draw_main_window_row(&mut self,
                            row: u16,
                            highlight: bool,
                            search_match_len: Option<u16>) -> CTResult<()> {
        let row_abs = row  + HEADER_SIZE;
        let w: usize = main_window_size()?.0.into();

        let (item, bold) = self.get_item_at_row(row).map_or(
            ("".to_string(), false),
            |itm| (itm.file_name_checked(), itm.is_dir())
        );
        let item_size = item.len();

        let attr = if bold {
            Attribute::Bold
        } else {
            Attribute::Dim
        };

        self.queue_clear_row(row_abs);

        queue!(
            self.window,
            cursor::MoveTo(0, row_abs),
            style::SetAttribute(Attribute::Reset),
            style::ResetColor,
            style::SetAttribute(attr),
        );

        if let Some(n) = search_match_len {
            // print underlined part
            let n = n as usize;
            let item_underline = item.get(..n).unwrap_or(&item);
            let item_no_underline = item.get(n..).unwrap_or("");
            queue!(
                self.window,
                style::SetAttribute(Attribute::Underlined),
                style::Print(item_underline.get(..w).unwrap_or(&item_underline)),
                style::SetAttribute(Attribute::NoUnderline),
            );
            if highlight {
                queue!(
                    self.window,
                    style::SetBackgroundColor(style::Color::White),
                    style::SetForegroundColor(style::Color::Black),
                )?;
            }
            queue!(
                self.window,
                style::Print(item_no_underline.get(..w.checked_sub(n).unwrap_or(0)).unwrap_or(&item_no_underline)),
                style::Print(" ".repeat(w.checked_sub(item_size).unwrap_or(0))),
            )?;

        } else {
            if highlight {
                queue!(
                    self.window,
                    style::SetBackgroundColor(style::Color::White),
                    style::SetForegroundColor(style::Color::Black),
                    style::Print(item.get(..w).unwrap_or(&item)),
                    style::Print(" ".repeat(w.checked_sub(item_size).unwrap_or(0))),
                )?;
            } else {
                queue!(
                    self.window,
                    style::Print(item.get(..w).unwrap_or(&item)),
                )?;
            }
        }
        execute!(
            self.window,
            style::ResetColor,
            style::SetAttribute(Attribute::Reset),
        )

    }

    // redraw row 'row' (relative to the top of the main window) without highlighting
    pub fn unhighlight_row(&mut self, row: u16) {
        let match_len = if self.app_state.is_searching() {
            Some(u16::try_from(self.app_state.search_string().len()).unwrap_or(u16::MAX))
        } else {
            None
        };
        self.draw_main_window_row(u16::try_from(row).unwrap_or(u16::MAX), false, match_len);
    }

    pub fn highlight_row(&mut self, row: u32) { //TODO: change row to u16
        // Highlight the row `row` in the main window. Row 0 is the first row of
        // the main window
        //TODO: underline search match...

        let (w, _) = main_window_size().unwrap(); //TODO: error handling
        let w = w as usize;
        let item = self.get_item_at_row(row as u16).map_or("".to_string(), |itm| itm.file_name_checked());
        let item_size = item.len();

        self.queue_clear_row(row as u16 + HEADER_SIZE);
        execute!(
            self.window,
            cursor::MoveTo(0, row as u16 + HEADER_SIZE),
            style::SetAttribute(Attribute::Reset),
            style::SetBackgroundColor(style::Color::White),
            style::SetForegroundColor(style::Color::Black),
            style::Print(item.get(..w).unwrap_or(&item)),
            style::Print(" ".repeat(w.checked_sub(item_size).unwrap_or(0))),
            style::ResetColor,
        );
    }

    fn queue_clear_main_window(&mut self) -> CTResult<()> {
        let (_, h) = main_window_size()?;
        for row in HEADER_SIZE..h+HEADER_SIZE {
            self.queue_clear_row(row)?;
        }
        Ok(())
    }

    pub fn highlight_row_exclusive(&mut self, row: u32) {
        // Highlight the row `row` exclusively, and hide all other rows.
        self.queue_clear_main_window();
        self.highlight_row(row);
    }

    pub fn redraw_main_window(&mut self) -> CTResult<()> {

        let (_, max_y) = main_window_size()?;
        let scroll_pos = self.app_state.scroll_pos;
        let mut win = self.window;

        let match_indices: std::collections::HashSet<usize> = self.app_state
            .search_matches().iter().map(|(i, _)| *i).collect();

        self.queue_clear_main_window();

        // draw entries
        let all_lines = self.app_state.ls_output_buf.iter();
        for (view_idx, (buf_idx, entry)) in all_lines.enumerate().skip(scroll_pos as usize)
            .enumerate().take(max_y as usize) {
                //TODO: show  modified date and other info (should query metadata already in update_ls_output_buf)
                let row = view_idx as u16 + HEADER_SIZE;

                let attr = if entry.is_dir() {
                    Attribute::Bold
                } else {
                    Attribute::Dim
                };

                let line = entry.file_name_checked();

                let match_len = if match_indices.contains(&buf_idx) {
                    self.app_state.search_string().len()
                } else {
                    0
                };

                queue!(
                    win,
                    cursor::MoveTo(0, row),
                    style::SetAttribute(Attribute::Reset),
                    style::SetAttribute(attr),
                    style::SetAttribute(Attribute::Underlined),
                    style::Print(line.get(..match_len).unwrap_or(&line)),
                    style::SetAttribute(Attribute::NoUnderline),
                    style::Print(line.get(match_len..).unwrap_or("")),
                );
        }

        // show "cursor"
        self.highlight_row(self.app_state.cursor_pos);

        //TODO: do underlining of matches only after highlight? like originally?

        win.flush()
    }

    fn redraw_all_windows(&mut self) {
        self.redraw_header();
        self.redraw_info_window();
        self.redraw_footer();
        self.redraw_main_window();
    }

    /// Update the app state by moving the cursor by the specified amount, and
    /// redraw the view as necessary.
    pub fn move_cursor(&mut self, amount: i32, wrap: bool) {
        self.unhighlight_row(u16::try_from(self.app_state.cursor_pos).unwrap_or(u16::MAX));

        let old_scroll_pos = self.app_state.scroll_pos;

        self.app_state.move_cursor(amount, wrap);

        if self.app_state.scroll_pos != old_scroll_pos {
            // redraw_main_window takes care of highlighting the cursor row
            // and refreshing
            self.redraw_main_window();
        } else {
            self.highlight_row(self.app_state.cursor_pos);
        }
    }

    pub fn change_dir(&mut self, path: &str) {
        match self.app_state.change_dir(path) {
            Err(e) => {
                if cfg!(debug_assertions) {
                    self.error_message(&format!("{:?}", e));
                } else {
                    self.error_message(&format!("{}", e));
                }
            },
            Ok(()) => {
                self.update_header();
                self.info_message("");
            }
        }
        self.redraw_main_window();
        self.redraw_footer();
    }

    pub fn on_search_char(&mut self, c: char) {
        self.app_state.advance_search(&c.to_string());
        if self.app_state.search_matches().len() == 1 {
            // There's only one match, highlight it and then change dir
            self.highlight_row_exclusive(self.app_state.cursor_pos);

            //TODO: make duration configurable
            std::thread::sleep(std::time::Duration::from_millis(200));

            // ignore keys that were pressed during sleep
            while crossterm::event::poll(std::time::Duration::from_secs(0))
                .unwrap_or(false) {
                read_event();
            }

            self.change_dir("");
        }
        self.redraw_main_window();
        self.redraw_footer();
    }

    pub fn erase_search_char(&mut self) {
        self.app_state.erase_search_char();
        self.redraw_main_window();
        self.redraw_footer();
    }

    pub fn on_resize(&mut self) -> Result<(), TereError> {

        let (w, h) = main_window_size()?;
        let (w, h) = (w as u32, h as u32);
        self.app_state.update_main_window_dimensions(w, h);

        self.redraw_all_windows();
        Ok(())
    }

    pub fn on_arrow_key(&mut self, up: bool) {
        let dir = if up { -1 } else { 1 };
        if self.app_state.is_searching() {
            //TODO: handle case where 'is_searching' but there are no matches - move cursor?
            self.app_state.move_cursor_to_adjacent_match(dir);
            self.redraw_main_window();
        } else {
            self.move_cursor(dir, true);
        }
        self.redraw_footer();
    }

    // When the 'page up' or 'page down' keys are pressed
    pub fn on_page_up_down(&mut self, up: bool) {
        if !self.app_state.is_searching() {
            let (_, h) = main_window_size().unwrap(); //TODO: error handling...
            let delta = ((h - 1) as i32)* if up { -1 } else { 1 };
            self.move_cursor(delta, false);
            self.redraw_footer();
        } //TODO: how to handle page up / page down while searching? jump to the next match below view?
    }

    // on 'home' or 'end'
    fn on_home_end(&mut self, home: bool) {
        if !self.app_state.is_searching() {
            let target = if home {
                0
            } else {
                self.app_state.ls_output_buf.len() as u32
            };
            self.app_state.move_cursor_to(target);
            self.redraw_main_window();
        } // TODO: else jump to first/last match
    }

    pub fn main_event_loop(&mut self) -> Result<(), TereError> {
        let ALT = KeyModifiers::ALT;
        let CONTROL = KeyModifiers::CONTROL;
        // root_win is the window created by initscr()
        loop {
            match read_event()? {
                Event::Key(k) => {
                    match k.code {
                        KeyCode::Right | KeyCode::Enter => self.change_dir(""),
                        KeyCode::Left => self.change_dir(".."),
                        KeyCode::Up if k.modifiers == ALT => {
                            self.change_dir("..");
                        },
                        KeyCode::Up => self.on_arrow_key(true),
                        KeyCode::Down if k.modifiers == ALT => {
                            self.change_dir("");
                        },
                        KeyCode::Down => self.on_arrow_key(false),

                        KeyCode::PageUp => self.on_page_up_down(true),
                        KeyCode::PageDown => self.on_page_up_down(false),

                        KeyCode::Home if k.modifiers == CONTROL => {
                            if let Some(path) = home_dir() {
                                if let Some(path) = path.to_str() {
                                    self.change_dir(path);
                                }
                            }
                        }

                        KeyCode::Home => self.on_home_end(true),
                        KeyCode::End => self.on_home_end(false),

                        KeyCode::Esc => {
                            if self.app_state.is_searching() {
                                self.app_state.clear_search();
                                self.redraw_main_window();
                                self.redraw_footer();
                            } else {
                                break;
                            }
                        },

                        // alt + hjkl
                        KeyCode::Char('h') if k.modifiers == ALT => {
                            self.change_dir("..");
                        }
                        KeyCode::Char('j') if k.modifiers == ALT => {
                            self.on_arrow_key(false);
                        }
                        KeyCode::Char('k') if k.modifiers == ALT => {
                            self.on_arrow_key(true);
                        }
                        KeyCode::Char('l') if k.modifiers == ALT => {
                            self.change_dir("");
                        }

                        // other chars with modifiers
                        KeyCode::Char('q') if k.modifiers == ALT => {
                            break;
                        }
                        KeyCode::Char('u') if (k.modifiers == ALT || k.modifiers == CONTROL) => {
                            self.on_page_up_down(true);
                        }
                        KeyCode::Char('d') if (k.modifiers == ALT || k.modifiers == CONTROL) => {
                            self.on_page_up_down(false);
                        }
                        KeyCode::Char('g') if k.modifiers == ALT => {
                            // like vim 'gg'
                            self.on_home_end(true);
                        }
                        KeyCode::Char('G') if k.modifiers.contains(ALT) => {
                            self.on_home_end(false);
                        }

                        KeyCode::Char(c) => self.on_search_char(c),

                        KeyCode::Backspace => self.erase_search_char(),

                        _ => self.info_message(&format!("{:?}", k)),
                    }
                },

                Event::Resize(_, _) => self.on_resize()?,

                //TODO don't show this in release
                e => self.info_message(&format!("{:?}", e)),
            }
        }

        Ok(())
    }
}

fn main() -> crossterm::Result<()> {

    let cli_args = App::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        //.author(env!("CARGO_PKG_AUTHORS")) // TODO: rest of these https://stackoverflow.com/a/27841363
        .arg(Arg::with_name("folders-only")
             .long("folders-only")
             //.short("f")  // TODO: check conflicts
             .help("only show folders in listing")
             )
        .get_matches();

    let mut stderr = std::io::stderr();

    execute!(
        stderr,
        terminal::EnterAlternateScreen,
        cursor::Hide,
    )?;

    stderr.flush();

    terminal::enable_raw_mode()?;

    let res = TereTui::init(&cli_args, &mut stderr)
        .map_err(|e| format!("error in initializing UI: {:?}", e))
        .and_then(|mut ui| ui.main_event_loop()
            .map_err(|e| format!("error in main event loop: {:?}", e))
        );

    // TODO: clean up even if there was an error
    execute!(
        stderr,
        terminal::LeaveAlternateScreen,
        cursor::Show
        )?;

    terminal::disable_raw_mode()?;

    // panic if there was an error
    res.unwrap();

    // no error, print cwd
    let cwd = std::env::current_dir().expect("error getting cwd");
    println!("{}", cwd.display());

    Ok(())
}
