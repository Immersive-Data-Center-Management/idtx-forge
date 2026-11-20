// Per-user widget login. Pair with templates/widgets/auth_login.usda.
// Grant widget, stage_edit, net, auth_write and level_control. Configure this
// endpoint, explicit runtime auth_origins and the remote/login level IDs.
// Credentials are entered at runtime and retained only in per-instance widget
// input storage. Password text is never authored into USD or carried state.
use idtx_behavior_sdk::prelude::*;

const LOGIN_URL: &str = "https://core.example.local/api/v1/auth/login";
const REMOTE_LEVEL: &str = "remote";
const LOGIN_LEVEL: &str = "login";

#[derive(Default)]
pub struct AuthLogin {
    pending: bool,
    cancelled: bool,
    password: bool,
    character: u8,
    auth_revision: u64,
}

#[behavior(kind = "runtime")]
impl AuthLogin {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        self.character = b'a';
        #[cfg(target_arch = "wasm32")]
        for name in ["Username", "Password", "Previous", "Next", "Append", "Backspace", "Login", "Cancel", "Logout", "Return"] {
            ctx.widget().on_click(name, Self::clicked)?;
        }
        self.publish(ctx)
    }

    fn clicked(&mut self, ctx: &Ctx, ev: &WidgetClick) -> Result<()> {
        self.command(ctx, ev.element().as_str())
    }

    fn command(&mut self, ctx: &Ctx, name: &str) -> Result<()> {
        match name {
            "Username" => self.password = false,
            "Password" => self.password = true,
            "Previous" => self.character = if self.character <= 32 { 126 } else { self.character - 1 },
            "Next" => self.character = if self.character >= 126 { 32 } else { self.character + 1 },
            "Append" | "Backspace" if !self.pending => {
                let field = if self.password { "password" } else { "username" };
                let mut value = ctx.widget().value(field)?;
                if name == "Backspace" { value.pop(); }
                else if value.len() < 128 { let _ = value.try_push(char::from(self.character)); }
                ctx.widget().set_value(field, &value)?;
            }
            "Login" if !self.pending => {
                let user = ctx.widget().value("username")?;
                let pass = ctx.widget().value("password")?;
                if user.is_empty() || pass.is_empty() { return ctx.stage_edit().set_widget_value("Status", "Enter username and password"); }
                let mut json = [0u8; 2048];
                let Some(body) = login_json(&user, &pass, &mut json) else { return Ok(()); };
                self.auth_revision = auth::revision();
                self.cancelled = false;
                net::post_json_async(LOGIN_URL, body, Self::on_login)?;
                self.pending = true;
                ctx.widget().set_value("password", "")?;
                ctx.stage_edit().set_widget_value("Status", "Signing in")?;
            }
            "Cancel" | "Logout" | "Return" => {
                self.cancelled = true;
                // Release the single-flight latch immediately so the user can
                // start a fresh sign-in at once instead of waiting out the
                // in-flight request's deadline (up to 30 s). `on_login` still
                // discards the cancelled request's eventual response via the
                // `cancelled` guard, so a cancelled sign-in never completes.
                self.pending = false;
                ctx.widget().set_value("password", "")?;
                if name != "Return" { ctx.level().cancel()?; }
                if name == "Logout" { auth::clear()?; }
                if name == "Return" { ctx.level().goto(LOGIN_LEVEL)?; }
                ctx.stage_edit().set_widget_value("Status", "Cancelled / signed out")?;
            }
            _ => {}
        }
        self.publish(ctx)
    }

    fn publish(&self, ctx: &Ctx) -> Result<()> {
        ctx.stage_edit().set_widget_value("UsernameValue", &ctx.widget().value("username")?)?;
        let password = ctx.widget().value("password")?;
        let mask = [b'*'; 128];
        ctx.stage_edit().set_widget_value("PasswordValue", core::str::from_utf8(&mask[..password.len().min(128)]).unwrap_or(""))?;
        let character = [self.character];
        ctx.stage_edit().set_widget_value("Character", core::str::from_utf8(&character).unwrap_or("a"))?;
        ctx.stage_edit().set_widget_value("Field", if self.password { "Editing password" } else { "Editing username" })
    }

    fn on_login(&mut self, ctx: &Ctx, resp: HttpResponse) -> Result<()> {
        self.pending = false;
        if self.cancelled || auth::revision() != self.auth_revision { return Ok(()); }
        if !resp.is_success() { return ctx.stage_edit().set_widget_value("Status", "Login failed; enter password and retry"); }
        let mut buf = [0u8; 4096];
        let n = resp.read_body(&mut buf);
        // Never accept a token extracted from a truncated response.
        if n > buf.len() { return ctx.stage_edit().set_widget_value("Status", "Login response too large"); }
        if let Some(token) = net::json_string_field(&buf[..n], "access_token") {
            auth::set_token(token)?;
            ctx.stage_edit().set_widget_value("Status", "Signed in")?;
            ctx.level().goto(REMOTE_LEVEL)?;
        } else { ctx.stage_edit().set_widget_value("Status", "Login response missing access_token")?; }
        Ok(())
    }
}

// Bounded JSON escaping (including quotes, backslashes and control characters).
fn login_json<'a>(user: &str, password: &str, out: &'a mut [u8]) -> Option<&'a str> {
    let mut n = 0usize;
    let mut push = |bytes: &[u8]| -> Option<()> {
        out.get_mut(n..n.checked_add(bytes.len())?)?.copy_from_slice(bytes);
        n += bytes.len(); Some(())
    };
    push(b"{\"username\":\"")?;
    for (i, value) in [user, password].iter().enumerate() {
        if i == 1 { push(b"\",\"password\":\"")?; }
        for byte in value.bytes() {
            match byte {
                b'"' => push(b"\\\"")?, b'\\' => push(b"\\\\")?,
                0..=31 => { const HEX: &[u8] = b"0123456789abcdef"; push(&[b'\\', b'u', b'0', b'0', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]])?; }
                _ => push(&[byte])?,
            }
        }
    }
    push(b"\"}")?;
    core::str::from_utf8(&out[..n]).ok()
}
