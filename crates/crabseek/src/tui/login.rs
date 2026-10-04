//! First-run login screen, also shown when saved credentials stop working.

use crabseek_net::{LoginError, StartError};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

/// The server's limit for usernames.
const MAX_USERNAME: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Username,
    Password,
}

pub enum LoginAction {
    None,
    Submit { username: String, password: String },
    Quit,
}

pub struct LoginForm {
    username: String,
    password: String,
    field: Field,
    pub error: Option<String>,
    pub busy: bool,
}

impl LoginForm {
    pub fn new(username: String) -> Self {
        let field = if username.is_empty() {
            Field::Username
        } else {
            Field::Password
        };
        Self {
            username,
            password: String::new(),
            field,
            error: None,
            busy: false,
        }
    }

    /// After a failed attempt: keep the username, ask for the password again.
    pub fn failed(&mut self, error: String) {
        self.error = Some(error);
        self.password.clear();
        self.field = Field::Password;
        self.busy = false;
    }

    pub fn on_key(&mut self, key: KeyEvent) -> LoginAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
            return LoginAction::Quit;
        }
        if self.busy {
            return LoginAction::None;
        }
        let text = match self.field {
            Field::Username => &mut self.username,
            Field::Password => &mut self.password,
        };
        match key.code {
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                self.field = match self.field {
                    Field::Username => Field::Password,
                    Field::Password => Field::Username,
                }
            }
            KeyCode::Enter => {
                if self.field == Field::Username && self.password.is_empty() {
                    self.field = Field::Password;
                    return LoginAction::None;
                }
                match validate(&self.username, &self.password) {
                    Ok(()) => {
                        return LoginAction::Submit {
                            username: self.username.clone(),
                            password: self.password.clone(),
                        };
                    }
                    Err(e) => self.error = Some(e.to_owned()),
                }
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char('u') if ctrl => text.clear(),
            KeyCode::Char(c) if !ctrl => text.push(c),
            _ => {}
        }
        LoginAction::None
    }
}

/// The server's username rules (see "Login Rejection Details" in the spec),
/// checked up front so a typo does not register a broken account name.
fn validate(username: &str, password: &str) -> Result<(), &'static str> {
    if username.is_empty() {
        return Err("Enter a username.");
    }
    if username.chars().count() > MAX_USERNAME {
        return Err("Usernames can be at most 30 characters.");
    }
    if !username
        .chars()
        .all(|c| c.is_ascii() && !c.is_ascii_control())
    {
        return Err("Usernames may only contain printable ASCII characters.");
    }
    if username.trim() != username {
        return Err("Usernames cannot start or end with a space.");
    }
    if password.is_empty() {
        return Err("Enter a password.");
    }
    Ok(())
}

/// Whether the server refused these credentials, as opposed to the
/// network or the listen port failing; only then is the form needed again.
pub fn is_credential_error(error: &StartError) -> bool {
    matches!(
        error,
        StartError::Login(LoginError::Rejected { reason, .. })
            if reason == "INVALIDPASS" || reason == "INVALIDUSERNAME"
    )
}

/// A human explanation of why logging in failed.
pub fn explain(error: &StartError) -> String {
    match error {
        StartError::Login(LoginError::Rejected { reason, detail }) => match reason.as_str() {
            "INVALIDPASS" => "Wrong password for this username.".to_owned(),
            "INVALIDUSERNAME" => format!(
                "The server rejected this username{}.",
                detail
                    .as_ref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default()
            ),
            "SVRFULL" => "The server is full, try again later.".to_owned(),
            "SVRPRIVATE" => "The server does not accept new accounts right now.".to_owned(),
            "INVALIDVERSION" => "The server says this client is too old.".to_owned(),
            other => format!("Login rejected: {other}"),
        },
        StartError::Listen { port, .. } => format!(
            "Port {port} is already in use – is another Soulseek client running? \
             Change listen_port in the config to use a different one."
        ),
        other => other.to_string(),
    }
}

pub fn render(frame: &mut Frame, form: &LoginForm, config_path: &str) {
    let area = centered(frame.area(), 68, 17);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(" crabseek ".bold())
        .title_bottom(Line::from(" Tab switch field · Enter log in · Esc quit ").dark_gray())
        .border_style(Style::new().cyan());
    let inner = block.inner(area).inner(ratatui::layout::Margin::new(2, 1));
    frame.render_widget(block, area);

    let [
        title,
        _,
        user_label,
        user_box,
        pass_label,
        pass_box,
        _,
        hint,
        status,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(1),
    ])
    .areas(inner);

    frame.render_widget(Paragraph::new("Log in to Soulseek").bold(), title);

    let field_style = |f: Field| {
        if form.field == f && !form.busy {
            Style::new().reversed()
        } else {
            Style::new().on_dark_gray()
        }
    };
    frame.render_widget(Paragraph::new("Username").dark_gray(), user_label);
    frame.render_widget(
        Paragraph::new(format!(" {}", form.username)).style(field_style(Field::Username)),
        user_box,
    );
    frame.render_widget(Paragraph::new("Password").dark_gray(), pass_label);
    let masked = "•".repeat(form.password.chars().count());
    frame.render_widget(
        Paragraph::new(format!(" {masked}")).style(field_style(Field::Password)),
        pass_box,
    );

    frame.render_widget(
        Paragraph::new(format!(
            "New to Soulseek? Pick any free name: the server creates the account \
             on first login, so type it carefully. Saved to {config_path} \
             (readable only by you)."
        ))
        .dark_gray()
        .wrap(Wrap { trim: true }),
        hint,
    );

    let status_line = if form.busy {
        Line::from(format!("Connecting as {}...", form.username)).yellow()
    } else if let Some(error) = &form.error {
        Line::from(error.as_str()).red()
    } else {
        Line::default()
    };
    frame.render_widget(
        Paragraph::new(status_line).wrap(Wrap { trim: true }),
        status,
    );

    if !form.busy {
        let (row, len) = match form.field {
            Field::Username => (user_box, form.username.width()),
            Field::Password => (pass_box, form.password.chars().count()),
        };
        let x = (row.x + 1 + len as u16).min(row.right().saturating_sub(1));
        frame.set_cursor_position(Position::new(x, row.y));
    }
}

/// Shown while logging in with saved credentials, and when that fails for
/// a reason other than the credentials.
pub fn render_splash(frame: &mut Frame, username: &str, error: Option<&str>) {
    let area = centered(frame.area(), 60, 7);
    frame.render_widget(Clear, area);
    let hint = if error.is_some() {
        " r retry · q quit "
    } else {
        " q quit "
    };
    let block = Block::bordered()
        .title(" crabseek ".bold())
        .title_bottom(Line::from(hint).dark_gray())
        .border_style(Style::new().cyan());
    let inner = block.inner(area).inner(ratatui::layout::Margin::new(2, 1));
    frame.render_widget(block, area);
    let text = match error {
        None => Line::from(format!("Connecting as {username}...")).yellow(),
        Some(e) => Line::from(e).red(),
    };
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), inner);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(form: &mut LoginForm, code: KeyCode) -> LoginAction {
        form.on_key(KeyEvent::from(code))
    }

    fn type_text(form: &mut LoginForm, text: &str) {
        for c in text.chars() {
            press(form, KeyCode::Char(c));
        }
    }

    #[test]
    fn username_rules() {
        assert!(validate("alice", "pw").is_ok());
        assert!(validate("", "pw").is_err());
        assert!(validate(" alice", "pw").is_err());
        assert!(validate("árvíz", "pw").is_err());
        assert!(validate(&"a".repeat(31), "pw").is_err());
        assert!(validate("alice", "").is_err());
    }

    #[test]
    fn enter_moves_to_password_then_submits() {
        let mut form = LoginForm::new(String::new());
        type_text(&mut form, "alice");
        assert!(matches!(
            press(&mut form, KeyCode::Enter),
            LoginAction::None
        ));
        type_text(&mut form, "secret");
        match press(&mut form, KeyCode::Enter) {
            LoginAction::Submit { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "secret");
            }
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn failure_keeps_username_and_clears_password() {
        let mut form = LoginForm::new("alice".into());
        type_text(&mut form, "wrong");
        form.failed("Wrong password for this username.".into());
        assert_eq!(form.username, "alice");
        assert!(form.password.is_empty());
        assert_eq!(form.field, Field::Password);
    }

    #[test]
    fn esc_quits() {
        let mut form = LoginForm::new(String::new());
        assert!(matches!(press(&mut form, KeyCode::Esc), LoginAction::Quit));
    }
}
