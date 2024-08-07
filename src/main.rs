use kdl::KdlDocument;
use std::time::Instant;
use std::path::PathBuf;
use std::fs::{self, File};
use std::io::prelude::*;
use zellij_tile::prelude::*;

use std::collections::{HashMap, BTreeMap};

#[derive(Default)]
struct State {
    current_run_index: usize,
    userspace_configuration: BTreeMap<String, String>,
    commands_to_run: Vec<Command>,
    active_edit_pane_ids: Vec<u32>,
    shell: String,
    folder: String,
    running_command_index: Option<usize>,
    selected_index: Option<usize>,
    paused: bool,
    stop_on_failure: bool,
}

register_plugin!(State);

#[derive(Debug)]
struct Command {
    command_line: String,
    start_time: Option<Instant>,
    end_time: Option<Instant>,
    pane_id: Option<PaneId>,
    exit_status: Option<i32>,
    exited: bool,
    pane_closed_by_user: bool,
}

impl Command {
    pub fn new<S: AsRef<str>>(command_line: S) -> Self
        where S: ToString,
    {
        Command {
            command_line: command_line.to_string(),
            start_time: None,
            end_time: None,
            pane_id: None,
            exit_status: None,
            exited: false,
            pane_closed_by_user: false,
        }
    }
    pub fn reset(&mut self) {
        *self = Self::new(&self.command_line);
    }
}

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.userspace_configuration = configuration;
        request_permission(&[PermissionType::ReadApplicationState, PermissionType::ChangeApplicationState, PermissionType::RunCommands, PermissionType::OpenFiles]);
        subscribe(&[
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
            EventType::CommandPaneOpened,
            EventType::CommandPaneExited,
            EventType::EditPaneOpened,
            EventType::EditPaneExited,
            EventType::Key,
            EventType::Timer,
            EventType::PaneClosed,
        ]);

        let mut commands_to_run = vec![];
        eprintln!("userspace_configuration: {:?}", self.userspace_configuration);
        if let Some(commands) = self.userspace_configuration.get("commands") {
            if let Ok(doc) = commands.parse::<KdlDocument>() {
                // commands are in kdl format
                for node in doc.nodes() {
                    commands_to_run.push(Command::new(node.name().value().trim()));
                }
            } else {
                for command in commands.split("&&") {
                    commands_to_run.push(Command::new(command.trim()));
                }
            }
        }

        self.shell = self.userspace_configuration.get("shell").map(|s| s.to_string()).unwrap_or_else(|| "bash".to_string());
        self.folder = self.userspace_configuration.get("folder").map(|s| s.to_string()).unwrap_or_else(|| ".".to_string());

        // TODO:
        // * cleanups (close terminals on re-run... anything else?) - DONE
        // * if terminal is closed, re-run the command when pressing TAB - DONE
        // * allow to stop after failed command with config or smth - DONE
        // * allow to pause run - DONE
        // * "re-run from here" - Not now
        // * receive commands from command line - DONE
        // * show total run time - DONE
        // * deal with cwd - DONE
        // * deal with shell - DONE
        // * change commands at runtime <--- CONTINUE HERE (25/07)


        // mock data
        let commands_to_run = vec![
            Command::new("curl https://get.volta.sh | bash"),
            Command::new("volta install pnpm"),
            Command::new("pnpm install"),
            Command::new("docker-compose up -d"),
        ];
        self.commands_to_run = commands_to_run;
        self.running_command_index = None;
        set_timeout(1.0);
    }
//     fn pipe (&mut self, pipe_message: PipeMessage) -> bool {
//         eprintln!("pipe_message: {:?}", pipe_message);
//         true
//     }
    fn update(&mut self, event: Event) -> bool {
        let mut should_render = false;
        match event {
            Event::Timer(_elapsed) => {
                set_timeout(1.0);
                should_render = true;
            }
            Event::PermissionRequestResult(result) => {
                if result == PermissionStatus::Granted && self.running_command_index == None {
                    self.current_run_index += 1;
                    self.run_next_command();
                }
                should_render = true;
            }
            Event::CommandPaneOpened(terminal_pane_id, context) => {
                eprintln!("CommandPaneOpened: {:?} -> {:?}", terminal_pane_id, context);
                let command_index = context.get("command_index").and_then(|i| i.parse::<usize>().ok());
                let current_run_index = context.get("current_run_index").and_then(|i| i.parse::<usize>().ok());
                match (command_index, current_run_index) {
                    (Some(command_index), Some(current_run_index)) => {
                        if current_run_index == self.current_run_index {
                            if let Some(command) = self.commands_to_run.get_mut(command_index) {
                                command.pane_id = Some(PaneId::Terminal(terminal_pane_id));
                                command.start_time = Some(Instant::now());
                                should_render = true;
                            }
                        } else {
                            eprintln!("Received a message from a previous run, ignoring");
                        }
                    }
                    _ => {}
                }
            }
            Event::CommandPaneExited(terminal_pane_id, exit_code, context) => {
                eprintln!("CommandPaneExited: {:?} -> {:?}, {:?}", terminal_pane_id, exit_code, context);

                let command_index = context.get("command_index").and_then(|i| i.parse::<usize>().ok());
                let current_run_index = context.get("current_run_index").and_then(|i| i.parse::<usize>().ok());
                match (command_index, current_run_index) {
                    (Some(command_index), Some(current_run_index)) => {
                        if current_run_index == self.current_run_index {
                            if let Some(command) = self.commands_to_run.get_mut(command_index) {
                                command.exit_status = exit_code;
                                command.exited = true;
                                command.end_time = Some(Instant::now());
                                if let Some(pane_id) = command.pane_id {
                                    hide_pane_with_id(pane_id);
                                }
                                if self.running_command_index == Some(command_index) {
                                    self.run_next_command();
                                }
                            }
                        } else {
                            eprintln!("Received a message from a previous run, ignoring");
                        }
                    },
                    _ => {}
                }
                should_render = true;

            }
            Event::EditPaneOpened(terminal_pane_id, context) => {
                eprintln!("EditPaneOpened: {:?} -> {:?}", terminal_pane_id, context);
                if context.get("edit_pane_marker").is_some() {
                    self.active_edit_pane_ids.push(terminal_pane_id);
                }
            }
            Event::EditPaneExited(terminal_pane_id, exit_code, context) => {
                eprintln!("EditPaneExited: {:?} -> {:?}, {:?}", terminal_pane_id, exit_code, context);
                if context.get("edit_pane_marker").is_some() {
                    self.active_edit_pane_ids.retain(|p| *p != terminal_pane_id);
                    self.handle_editor_closed();
                }
            }
            Event::PaneClosed(pane_id) => {
                for command in self.commands_to_run.iter_mut() {
                    if command.pane_id == Some(pane_id) {
                        *command = Command::new(&command.command_line);
                        command.pane_closed_by_user = true;
                        should_render = true;
                        break;
                    }
                }
                if let PaneId::Terminal(terminal_pane_id) = pane_id {
                    if self.active_edit_pane_ids.contains(&terminal_pane_id) {
                        self.active_edit_pane_ids.retain(|p| *p != terminal_pane_id);
                        self.handle_editor_closed();
                    }
                }
            }
            Event::Key(key) => {
                if key.bare_key == BareKey::Down && key.has_no_modifiers() {
                    self.move_selection_down();
                    should_render = true;
                } else if key.bare_key == BareKey::Up && key.has_no_modifiers() {
                    self.move_selection_up();
                    should_render = true;
                } else if key.bare_key == BareKey::Tab && key.has_no_modifiers() {
                    self.focus_selected_terminal();
                    should_render = true;
                } else if key.bare_key == BareKey::Enter && key.has_no_modifiers() {
                    self.restart_run();
                    should_render = true;
                } else if key.bare_key == BareKey::Char(' ') && key.has_no_modifiers() {
                    self.paused = !self.paused;
                    if !self.paused && !self.all_commands_exited() {
                        self.run_next_command();
                    }
                    should_render = true;
                } else if key.bare_key == BareKey::Char('f') && key.has_no_modifiers() {
                    self.stop_on_failure = !self.stop_on_failure;
                    if !self.stop_on_failure && !self.all_commands_exited() {
                        self.run_next_command();
                    }
                    should_render = true;
                } else if key.bare_key == BareKey::Char('e') && key.has_no_modifiers() {
                    self.open_editor();
                    should_render = true;
                }
            }
            _ => (),
        };
        should_render
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let title = "Waiting to run 4 commands...";
        let title = "Running 1/4 commands (Success: 0, Failure: 0)";
        let title = "Stopped (Success: 0, Failure: 0, Did not run: 2)";
        let title = "Finished (Success: 0, Failure: 0)";

        let title        = "Running 1/4 commands (Success: 0, Failure: 0)";

        let list_item_1 = " > curl https://get.volta.sh | bash (Running...)";
        let list_item_1 = "   - Running for: 1.5s";
        let list_item_1 = "   - <ENTER> Open terminal";
        let list_item_2 = " > volta install pnpm";
        let list_item_3 = " > pnpm install";
        let list_item_4 = " > docker-compose up -d";

        let title = self.render_title(rows, cols);
        let mut list = vec![];
        for (i, command) in self.commands_to_run.iter().enumerate() {
            // let is_running = Some(i) == self.running_command_index;
            let is_running = command.start_time.is_some() && command.end_time.is_none();
            let is_selected = Some(i) == self.selected_index;
            list.append(&mut self.render_command(command, is_running, is_selected));
        }
        print_text_with_coordinates(title, 1, 1, None, None);
        print_nested_list_with_coordinates(list, 0, 3, Some(cols), None);
        self.render_status(rows, cols);
        self.render_help(rows, cols);
    }
}

impl State {
    fn kill_all_commands(&mut self) {
        for command in self.commands_to_run.iter_mut() {
            if let Some(PaneId::Terminal(pane_id)) = command.pane_id {
                close_terminal_pane(pane_id);
            }
        }
    }
    fn handle_editor_closed(&mut self) {
        match fs::read_to_string("/host/.editing-commands") {
            Ok(new_commands) => {
                // TODO: also delete the file
                self.kill_all_commands();
                self.commands_to_run = new_commands.trim().split('\n').map(|c| Command::new(c)).collect();
                eprintln!("commands_to_run: {:#?}", self.commands_to_run);
                self.running_command_index = None;
                self.current_run_index += 1;
                self.run_next_command();
            },
            Err(e) => {
                eprintln!("Failed to read commands: {}", e);
            }
        }
    }
    fn open_editor(&mut self) {
        let stringified_commands: Vec<String> = self.commands_to_run.iter().map(|c| c.command_line.to_string()).collect();
        let stringified_commands = stringified_commands.join("\n");
        match File::create("/host/.editing-commands").and_then(|mut file| file.write_all(stringified_commands.as_bytes())) {
            Ok(_) => {
                let mut context = BTreeMap::new();
                context.insert("edit_pane_marker".into(), String::new());
                open_file_floating(FileToOpen::new(".editing-commands"), None, context);
            }
            Err(e) => {
                eprintln!("Failed to write commands file: {}", e);
            }
        }
        // TODO:
        // * dump the commands into a file - DONE
        // * open an editor pane to that file - DONE
        // * when the editor pane closes, parse the contents of the file into commands and replace
        // the current ones with it
    }
    fn restart_run(&mut self) {
        self.running_command_index = None;
        for command in self.commands_to_run.iter_mut() {
            if let Some(PaneId::Terminal(pane_id)) = command.pane_id {
                close_terminal_pane(pane_id);
            }
            *command = Command::new(&command.command_line);
        }
        self.current_run_index += 1;
        self.run_next_command();
    }
    fn render_status(&self, rows: usize, cols: usize) {
        let y_coords = 6 + self.commands_to_run.len();
        let shell_text = self.shell.to_string();
        let folder_text = self.folder.to_string();
        let total_run_time = self.total_run_time();
        let text = format!("Elapsed: {}s Shell: {} Folder: {}", total_run_time, shell_text, folder_text);
        let text = Text::new(text)
            .color_range(1, 9..10 + total_run_time.chars().count())
            .color_range(1, 18 + total_run_time.chars().count()..19 + total_run_time.chars().count() + shell_text.chars().count())
            .color_range(1, 26 + total_run_time.chars().count() + shell_text.chars().count()..27 + total_run_time.chars().count() + shell_text.chars().count() + folder_text.chars().count());
//         let text = Text::new(text)
//             .color_range(1, ..8)
//             .color_range(1, 11 + total_run_time.chars().count()..17 + total_run_time.chars().count())
//             .color_range(1, 23 + total_run_time.chars().count()..23 + total_run_time.chars().count() + shell_text.chars().count());
        print_text_with_coordinates(text, 1, y_coords, None, None);
    }
    fn total_run_time(&self) -> String {
        let start_time = self.commands_to_run.iter().next().and_then(|c| c.start_time.clone()).unwrap_or_else(|| Instant::now());
        let end_time = self.commands_to_run.iter().rev().next().and_then(|c| c.end_time.clone()).unwrap_or_else(|| Instant::now());
        end_time.duration_since(start_time).as_secs().to_string()
    }
    fn render_help(&self, rows: usize, cols: usize) {
        let y_coords = 8 + self.commands_to_run.len();
        let enter_text = "ENTER";
        let enter_element = Text::new(enter_text).color_range(2, ..);
        let enter_ribbon_text = "Restart";
        let enter_ribbon = Text::new(enter_ribbon_text);
        let space_text = "SPACE";
        let space_element = Text::new(space_text).color_range(2, ..);
        let space_ribbon_text = "Pause After Command";
        let space_ribbon = if self.paused {
            Text::new(space_ribbon_text).selected()
        } else {
            Text::new(space_ribbon_text)
        };
        let f_text = "f";
        let f_element = Text::new(f_text).color_range(2, ..);
        let f_ribbon_text = "Stop on Failure";
        let f_ribbon = if self.stop_on_failure {
            Text::new(f_ribbon_text).selected()
        } else {
            Text::new(f_ribbon_text)
        };
        let enter_text_x_coords = 1;
        let enter_ribbon_x_coords = enter_text_x_coords + enter_text.chars().count() + 1;

        let space_text_x_coords = enter_ribbon_x_coords + enter_ribbon_text.chars().count() + 5;
        let space_ribbon_x_coords = space_text_x_coords + space_text.chars().count() + 1;

        let f_text_x_coords = space_ribbon_x_coords + space_ribbon_text.chars().count() + 5;
        let f_ribbon_x_coords = f_text_x_coords + f_text.chars().count() + 1;

        print_text_with_coordinates(enter_element, enter_text_x_coords, y_coords, None, None);
        print_ribbon_with_coordinates(enter_ribbon, enter_ribbon_x_coords, y_coords, None, None);

        print_text_with_coordinates(space_element, space_text_x_coords, y_coords, None, None);
        print_ribbon_with_coordinates(space_ribbon, space_ribbon_x_coords, y_coords, None, None);

        print_text_with_coordinates(f_element, f_text_x_coords, y_coords, None, None);
        print_ribbon_with_coordinates(f_ribbon, f_ribbon_x_coords, y_coords, None, None);
    }
    fn current_command_failed(&self) -> bool {
        self.running_command_index.and_then(|i| self.commands_to_run.get(i)).map(|c| !(c.exited && c.exit_status == Some(0))).unwrap_or(false)
    }
    fn run_next_command(&mut self) {
        if self.paused {
            return;
        }
        if self.current_command_failed() && self.stop_on_failure {
            return;
        }
        let next_index = self.running_command_index.map(|i| i + 1).unwrap_or(0);
        match self.commands_to_run.get_mut(next_index) {
            Some(next_command) => {
                let mut context = BTreeMap::new();
                context.insert("command_index".to_owned(), next_index.to_string());
                context.insert("current_run_index".to_owned(), self.current_run_index.to_string());
                Self::run_command(&next_command, context, &self.shell, &self.folder);
                self.running_command_index = Some(next_index);
            },
            None => {
                self.running_command_index = None;
            }
        }
    }
    fn run_command(command: &Command, context: BTreeMap<String, String>, shell: &str, folder: &str) {
        // TODO: dear gods no
        let mut command_line = vec![ "-ic" ];
        command_line.push(&command.command_line);
        let mut command_to_run = CommandToRun::new_with_args(shell, command_line);
        command_to_run.cwd = Some(PathBuf::from(folder));
        open_command_pane_background(command_to_run, context);
        // open_command_pane_background(CommandToRun::new_with_args("bash", command_line), context);
    }
    fn render_title(&self, rows: usize, cols: usize) -> Text {
        let successful_commands = self.successful_command_count();
        let successful_commands_indication = format!("{}", successful_commands);
        let failed_commands = self.failed_command_count();
        let failed_commands_indication = format!("{}", failed_commands);
        let pending_commands = self.pending_command_count();
        let pending_commands_indication = format!("{}", pending_commands);
        if let Some(running_command_index) = self.running_command_index.as_ref() {
            let total_commands = successful_commands + failed_commands + pending_commands;
            let title = format!("Running {}/{} commands (Success: {}, Failure: {}, Pending: {})", running_command_index + 1, total_commands, successful_commands_indication, failed_commands_indication, pending_commands_indication);
            Text::new(title)
                .color_range(1, 0..20)
                .color_range(2, 31..31 + successful_commands_indication.chars().count())
                .color_range(3, 42 + successful_commands_indication.chars().count()..42 + failed_commands_indication.chars().count() + 1)
                .color_range(1, 54 + failed_commands_indication.chars().count()..54 + pending_commands_indication.chars().count() + 1)
        } else if self.all_commands_exited() {
            let title = format!("Done running commands. (Success: {}, Failure: {}, Pending: {})", successful_commands_indication, failed_commands_indication, pending_commands_indication);
            Text::new(title)
                .color_range(1, 0..22)
                .color_range(2, 33..33 + successful_commands_indication.chars().count())
                .color_range(3, 44 + successful_commands_indication.chars().count()..44 + failed_commands_indication.chars().count() + 1)
                .color_range(1, 56 + failed_commands_indication.chars().count()..56 + pending_commands_indication.chars().count() + 1)
        } else {
            // let title = format!("Waiting to run commands... (Success: {}, Failure: {}, Pending: {})", successful_commands_indication, failed_commands_indication, pending_commands_indication);
            let title = format!("Running commands. (Success: {}, Failure: {}, Pending: {})", successful_commands_indication, failed_commands_indication, pending_commands_indication);
            Text::new(title)
                .color_range(1, 0..17)
                .color_range(2, 27 + successful_commands_indication.chars().count()..27 + successful_commands_indication.chars().count() + 1)
                .color_range(3, 39 + successful_commands_indication.chars().count()..40 + failed_commands_indication.chars().count())
                .color_range(1, 51 + failed_commands_indication.chars().count()..51 + pending_commands_indication.chars().count() + 1)
        }
    }
    fn all_commands_exited(&self) -> bool {
        self.commands_to_run.iter().all(|c| c.exited || c.pane_closed_by_user)
    }
    fn successful_command_count(&self) -> usize {
        self.commands_to_run.iter().filter(|c| c.exit_status == Some(0)).count()
    }
    fn failed_command_count(&self) -> usize {
        self.commands_to_run.iter().filter(|c| c.exited && c.exit_status != Some(0)).count()
    }
    fn pending_command_count(&self) -> usize {
        self.commands_to_run.iter().filter(|c| !c.exited).count()
    }
    fn render_command(&self, command: &Command, is_running: bool, is_selected: bool) -> Vec<NestedListItem> {
        let item_title = if is_running {
            NestedListItem::new(format!("{} (Running for {}s)", &command.command_line, &command.start_time.unwrap_or_else(|| Instant::now()).elapsed().as_secs()))
                .color_range(0, 0..command.command_line.chars().count() + 1)
                .color_range(1, command.command_line.chars().count() + 1..)
        } else if let Some(exit_status) = command.exit_status {
            let exit_status_color = if exit_status == 0 { 2 } else { 3 };
            let command_len = command.command_line.chars().count();
            NestedListItem::new(format!("{} [EXIT CODE: {}]", command.command_line, exit_status))
                .color_range(0, 0..command_len + 1)
                .color_range(exit_status_color, command_len + 13..command_len + 14)
        } else if command.exited {
            let command_len = command.command_line.chars().count();
            NestedListItem::new(format!("{} [EXITED]", command.command_line))
                .color_range(0, 0..command_len + 1)
                .color_range(3, command_len + 2..command_len + 8)
        } else if command.pane_closed_by_user {
            let command_len = command.command_line.chars().count();
            NestedListItem::new(format!("{} [CLOSED]", command.command_line))
                .color_range(0, 0..command_len + 1)
                .color_range(3, command_len + 2..command_len + 8)
        } else {
            let command_len = command.command_line.chars().count();
            NestedListItem::new(&command.command_line)
                .color_range(0, 0..command_len + 1)
        };
        if is_selected {
            let start_time = command.start_time.unwrap_or_else(|| Instant::now());
            let end_time = command.end_time.unwrap_or_else(|| Instant::now());
            let running_line = if is_running {
                NestedListItem::new(format!("Running for: {}s", end_time.duration_since(start_time).as_secs())).indent(1).selected()
            } else {
                NestedListItem::new(format!("Done after: {}s", end_time.duration_since(start_time).as_secs())).indent(1).selected()
            };
            let has_pane_id = command.pane_id.is_some();
            // TODO: Also add <Ctrl c> - delete command and close terminal
            let rerun_or_open = if has_pane_id { 
                NestedListItem::new("<TAB> - open terminal").color_range(2, 0..5).indent(1).selected()
            } else {
                NestedListItem::new("<TAB> - re-run in new terminal").color_range(2, 0..5).indent(1).selected()
            };
            vec![
                item_title.selected(),
                running_line,
                rerun_or_open,
            ]
        } else {
            vec![item_title]
        }
    }
    fn move_selection_down(&mut self) {
        let max_selected_index = self.commands_to_run.len().saturating_sub(1);
        match self.selected_index.as_mut() {
            None if !self.commands_to_run.is_empty() => {
                self.selected_index = Some(0);
            },
            Some(current_index) if *current_index < max_selected_index => {
                *current_index += 1;
            }
            _ => {
                self.selected_index = None;
            }
        }
    }
    fn move_selection_up(&mut self) {
        let max_selected_index = self.commands_to_run.len().saturating_sub(1);
        match self.selected_index.as_mut() {
            None if !self.commands_to_run.is_empty() => {
                self.selected_index = Some(max_selected_index);
            },
            Some(current_index) if *current_index > 0 => {
                *current_index -= 1;
            }
            _ => {
                self.selected_index = None;
            }
        }
    }
    fn focus_selected_terminal(&mut self) {
        let selected_index = self.selected_index;
        let current_run_index = self.current_run_index;
        let shell = self.shell.clone();
        let folder = self.folder.clone();
        if let Some(focused_command) = self.get_focused_command() {
            match focused_command.pane_id {
                Some(PaneId::Terminal(pane_id)) => {
                    let should_float_if_hidden = true;
                    focus_terminal_pane(pane_id, should_float_if_hidden)
                },
                _ => {
                    let mut context = BTreeMap::new();
                    if let Some(selected_index) = selected_index {
                        context.insert("command_index".to_owned(), selected_index.to_string());
                    }
                    context.insert("current_run_index".to_owned(), current_run_index.to_string());
                    focused_command.reset();
                    Self::run_command(&focused_command, context, &shell, &folder);
                }
            }
        }
    }
    fn get_focused_command(&mut self) -> Option<&mut Command> {
        match self.selected_index {
            Some(selected_index) => {
                self.commands_to_run.get_mut(selected_index)
            },
            None => None
        }
    }
}
