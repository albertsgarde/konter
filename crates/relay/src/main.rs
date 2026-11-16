//! konter-relay: the server's Matrix relay (docs/server.md).
//!
//! Two commands, which share a lock so that only one of them works in the
//! clone at a time:
//!
//! - `konter-relay daily`, run by a timer: fetches and builds the books (with
//!   the `daily` command from the config), then leaves the digest in the
//!   outbox. It never talks to Matrix.
//! - `konter-relay run`, a service: posts what's in the outbox to the room,
//!   and hands the owner's replies to Claude in the clone, posting its answers.
//!
//! A conversation with Claude starts on the first reply after a digest, so
//! Claude only runs when the owner answers, and each new digest starts a new
//! conversation.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent};
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use matrix_sdk::{Client, Room};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(version, about = "Posts konter's daily digest to Matrix and hands replies to Claude")]
struct Cli {
    /// The config file (default: relay.toml in konter's config folder).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fetch and build the books, then leave the digest in the outbox for `run` to post.
    Daily,
    /// Stay connected: post the outbox, and answer the owner's messages through Claude.
    Run,
}

/// `relay.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// The bot's homeserver, e.g. https://matrix.example.org.
    homeserver: String,
    /// The bot's Matrix ID.
    user: OwnedUserId,
    /// The bot's password. Only used to log in the first time; the session is
    /// kept in the state folder after that, so it can be removed then.
    password: Option<String>,
    /// The one person the bot listens to.
    owner: OwnedUserId,
    /// The room it posts in and listens to: an encrypted room with the owner.
    room: OwnedRoomId,
    /// The server's clone of konter, with the books in it.
    clone: PathBuf,
    /// The daily run, in the clone.
    #[serde(default = "default_daily")]
    daily: String,
    /// Prints the digest, in the clone; the relay adds --json for the details.
    #[serde(default = "default_digest")]
    digest: String,
    /// The Claude Code command.
    #[serde(default = "default_claude")]
    claude: String,
    /// What the daily run waits to reach (host:port) before it starts, so a
    /// machine that has just woken up doesn't report a failed fetch. Empty:
    /// don't wait.
    #[serde(default = "default_wait_for")]
    wait_for: Vec<String>,
    /// Where the relay keeps its session, store, outbox and lock (default:
    /// $XDG_STATE_HOME/konter-relay, or ~/.local/state/konter-relay).
    state: Option<PathBuf>,
}

fn default_daily() -> String {
    "just pinned more daily".into()
}

fn default_digest() -> String {
    "just pinned more konter digest".into()
}

fn default_claude() -> String {
    "claude".into()
}

fn default_wait_for() -> Vec<String> {
    vec!["api.enablebanking.com:443".into()]
}

impl Config {
    fn load(path: Option<PathBuf>) -> Result<Config> {
        let path = match path {
            Some(path) => path,
            None => konter_repo::fetch::config_dir()?.join("relay.toml"),
        };
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut config: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.clone = expand_home(&config.clone);
        config.state = Some(match config.state.take() {
            Some(dir) => expand_home(&dir),
            None => state_home()?.join("konter-relay"),
        });
        Ok(config)
    }

    fn state(&self) -> &Path {
        self.state.as_deref().expect("set by load")
    }

    fn outbox(&self) -> PathBuf {
        self.state().join("outbox")
    }
}

fn state_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state"))
}

fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
    }
}

/// A digest waiting in the outbox, and then the one the conversation is about.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Digest {
    /// When the daily run made it (RFC 3339).
    made: String,
    /// The message to post.
    text: String,
    /// `konter digest --json`, which says which transaction each number is.
    json: Option<serde_json::Value>,
}

/// The conversation with Claude: the digest it's about and Claude's session.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Conversation {
    digest: Option<Digest>,
    /// None until the first reply to the digest.
    session: Option<String>,
}

impl Conversation {
    fn path(config: &Config) -> PathBuf {
        config.state().join("conversation.json")
    }

    fn load(config: &Config) -> Result<Conversation> {
        let path = Conversation::path(config);
        if !path.exists() {
            return Ok(Conversation::default());
        }
        Ok(serde_json::from_str(&std::fs::read_to_string(&path)?)?)
    }

    fn save(&self, config: &Config) -> Result<()> {
        write_private(&Conversation::path(config), &serde_json::to_vec_pretty(self)?)
    }
}

/// Written whole and then renamed into place, readable only by the owner.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().context("no parent folder")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Held while working in the clone, by the daily run and by Claude.
fn lock(config: &Config) -> Result<File> {
    std::fs::create_dir_all(config.state())?;
    let file = File::create(config.state().join("lock"))?;
    file.lock()?;
    Ok(file)
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(cli.config)?;
    match cli.command {
        Cmd::Daily => daily(&config),
        Cmd::Run => tokio::runtime::Runtime::new()?.block_on(run(config)),
    }
}

/// A shell command in the clone: whether it succeeded, stdout, and stderr.
fn shell(config: &Config, command: &str) -> Result<(bool, String, String)> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&config.clone)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {command}"))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// How long the daily run waits for the network before running anyway.
const WAIT_ONLINE: Duration = Duration::from_secs(30 * 60);

/// Wait until every address takes a connection, for at most `limit`. Returns
/// whether they all did.
fn wait_online(addrs: &[String], limit: Duration) -> bool {
    let start = Instant::now();
    let mut said = false;
    loop {
        let offline: Vec<&String> = addrs.iter().filter(|addr| !reachable(addr)).collect();
        if offline.is_empty() {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        if !said {
            let offline: Vec<&str> = offline.iter().map(|addr| addr.as_str()).collect();
            eprintln!("Waiting to reach {}.", offline.join(", "));
            said = true;
        }
        std::thread::sleep(Duration::from_secs(15));
    }
}

/// Whether `addr` (host:port) resolves and takes a TCP connection.
fn reachable(addr: &str) -> bool {
    addr.to_socket_addrs()
        .map(|mut found| found.any(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(10)).is_ok()))
        .unwrap_or(false)
}

fn daily(config: &Config) -> Result<()> {
    // Before the lock, so the relay can answer while this waits.
    if !wait_online(&config.wait_for, WAIT_ONLINE) {
        eprintln!("Still offline after {} minutes; running anyway.", WAIT_ONLINE.as_secs() / 60);
    }
    let _lock = lock(config)?;
    let mut text = String::new();
    let (ok, out, err) = shell(config, &config.daily)?;
    if !ok {
        // The fetch's own error; the digest only notices a failing bank once
        // its data is old.
        text.push_str(&format!(
            "The daily run failed (`{}`):\n\n{}\n\n",
            config.daily,
            last_lines(&format!("{out}{err}"), 30)
        ));
    }
    let (ok, digest, err) = shell(config, &config.digest)?;
    if ok {
        text.push_str(&digest);
    } else {
        text.push_str(&format!("`{}` failed:\n\n{}\n", config.digest, last_lines(&err, 30)));
    }
    let (ok, json, _) = shell(config, &format!("{} --json", config.digest))?;
    let json = if ok { serde_json::from_str(&json).ok() } else { None };
    if text.trim().is_empty() {
        eprintln!("Nothing to say today.");
        return Ok(());
    }
    let digest = Digest { made: now(), text: text.trim_end().to_string(), json };
    let name = format!("{}.json", digest.made.replace(':', ""));
    write_private(&config.outbox().join(name), &serde_json::to_vec_pretty(&digest)?)?;
    eprintln!("{}", digest.text);
    Ok(())
}

/// What the service's one worker does, in order.
enum Job {
    /// Post whatever is in the outbox.
    Outbox,
    /// A message from the owner.
    Message(String),
}

async fn run(config: Config) -> Result<()> {
    let client = login(&config).await?;
    // Sync once before listening, so messages from before the start aren't
    // answered again.
    let response = client.sync_once(SyncSettings::default()).await?;
    // An invite from before the start isn't an event any more, so the handler
    // below never sees it.
    for room in client.invited_rooms() {
        if room.room_id() == config.room && room.invite_details().await?.inviter_id == config.owner {
            room.join().await?;
        }
    }
    let (jobs, mut queue) = mpsc::unbounded_channel();

    let owner = config.owner.clone();
    let room_id = config.room.clone();
    let messages = jobs.clone();
    client.add_event_handler(move |event: OriginalSyncRoomMessageEvent, room: Room| {
        let (owner, room_id, messages) = (owner.clone(), room_id.clone(), messages.clone());
        async move {
            if room.room_id() != room_id || event.sender != owner {
                return;
            }
            if let MessageType::Text(text) = event.content.msgtype {
                let _ = messages.send(Job::Message(text.body));
            }
        }
    });
    let owner = config.owner.clone();
    let room_id = config.room.clone();
    client.add_event_handler(move |event: StrippedRoomMemberEvent, room: Room, client: Client| {
        let (owner, room_id) = (owner.clone(), room_id.clone());
        async move {
            let invited_me = client.user_id().is_some_and(|me| event.state_key == me);
            if invited_me
                && event.sender == owner
                && room.room_id() == room_id
                && let Err(e) = room.join().await
            {
                eprintln!("joining {room_id}: {e}");
            }
        }
    });

    let ticker = jobs.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if ticker.send(Job::Outbox).is_err() {
                break;
            }
        }
    });

    let worker_client = client.clone();
    let config = std::sync::Arc::new(config);
    let worker_config = config.clone();
    tokio::spawn(async move {
        while let Some(job) = queue.recv().await {
            if let Err(e) = work(&worker_client, &worker_config, job).await {
                eprintln!("error: {e:#}");
                if let Some(room) = worker_client.get_room(&worker_config.room) {
                    let _ = room.send(RoomMessageEventContent::text_plain(format!("Relay error: {e:#}"))).await;
                }
            }
        }
    });

    eprintln!("Listening in {} for {}.", config.room, config.owner);
    client.sync(SyncSettings::default().token(response.next_batch)).await?;
    Ok(())
}

async fn work(client: &Client, config: &std::sync::Arc<Config>, job: Job) -> Result<()> {
    let room = client
        .get_room(&config.room)
        .with_context(|| format!("the bot isn't in {}: invite it from {}", config.room, config.owner))?;
    match job {
        Job::Outbox => {
            for path in outbox(config)? {
                let digest: Digest = serde_json::from_slice(&std::fs::read(&path)?)?;
                room.send(RoomMessageEventContent::text_plain(&digest.text)).await?;
                // A new digest, a new conversation.
                Conversation { digest: Some(digest), session: None }.save(config)?;
                std::fs::remove_file(&path)?;
            }
        }
        Job::Message(text) => {
            room.typing_notice(true).await?;
            let worker = config.clone();
            let answer = tokio::task::spawn_blocking(move || answer(&worker, &text)).await?;
            room.typing_notice(false).await?;
            let answer = answer.unwrap_or_else(|e| format!("Claude didn't answer: {e:#}"));
            room.send(RoomMessageEventContent::text_markdown(answer)).await?;
        }
    }
    Ok(())
}

/// The outbox's digests, oldest first.
fn outbox(config: &Config) -> Result<Vec<PathBuf>> {
    let dir = config.outbox();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "json"));
    paths.sort();
    Ok(paths)
}

/// The first message about a digest carries the digest, so that "3" means
/// the third transaction in it.
fn prompt(conversation: &Conversation, message: &str) -> String {
    if conversation.session.is_some() {
        return message.to_string();
    }
    let mut prompt = format!(
        "A message from the owner of the books, sent over Matrix through konter-relay \
         (docs/server.md):\n\n{message}\n"
    );
    match &conversation.digest {
        Some(digest) => {
            prompt.push_str(&format!(
                "\nIt answers the digest posted at {}, if it refers to one:\n\n{}\n",
                digest.made, digest.text
            ));
            if let Some(json) = &digest.json {
                prompt.push_str(&format!(
                    "\nThe digest's details, with the transaction ID behind each number:\n\n{}\n",
                    serde_json::to_string_pretty(json).unwrap_or_default()
                ));
            }
        }
        None => prompt.push_str("\nNo digest has been posted since the relay started.\n"),
    }
    prompt
}

/// Claude's answer to a message, in the clone, continuing the conversation.
fn answer(config: &Config, message: &str) -> Result<String> {
    let _lock = lock(config)?;
    let mut conversation = Conversation::load(config)?;
    let mut command = Command::new(&config.claude);
    command.args(["-p", "--output-format", "json"]);
    if let Some(session) = &conversation.session {
        command.args(["--resume", session]);
    }
    let mut child = command
        .current_dir(&config.clone)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", config.claude))?;
    // Dropped once written, so Claude sees the end of the prompt.
    child.stdin.take().unwrap().write_all(prompt(&conversation, message).as_bytes())?;
    let out = child.wait_with_output()?;
    let result: ClaudeResult = serde_json::from_slice(&out.stdout).with_context(|| {
        format!("{} printed no result: {}", config.claude, last_lines(&String::from_utf8_lossy(&out.stderr), 10))
    })?;
    if let Some(session) = result.session_id {
        conversation.session = Some(session);
        conversation.save(config)?;
    }
    if result.is_error {
        bail!("{}", result.result.unwrap_or_else(|| "an error, without a message".into()));
    }
    Ok(result.result.unwrap_or_default())
}

/// What `claude -p --output-format json` prints.
#[derive(Debug, Deserialize)]
struct ClaudeResult {
    #[serde(default)]
    is_error: bool,
    result: Option<String>,
    session_id: Option<String>,
}

/// The bot's login, kept in the state folder with the store's passphrase.
#[derive(Serialize, Deserialize)]
struct Saved {
    passphrase: String,
    session: Option<MatrixSession>,
}

async fn login(config: &Config) -> Result<Client> {
    let path = config.state().join("session.json");
    let mut saved: Saved = if path.exists() {
        serde_json::from_slice(&std::fs::read(&path)?).with_context(|| format!("reading {}", path.display()))?
    } else {
        Saved { passphrase: random_hex()?, session: None }
    };
    let client = Client::builder()
        .homeserver_url(&config.homeserver)
        .sqlite_store(config.state().join("store"), Some(&saved.passphrase))
        .build()
        .await?;
    match saved.session.clone() {
        Some(session) => client.restore_session(session).await?,
        None => {
            let password = config.password.as_deref().context("no password in relay.toml for the first login")?;
            client
                .matrix_auth()
                .login_username(&config.user, password)
                .initial_device_display_name("konter-relay")
                .await?;
            saved.session = client.matrix_auth().session();
            write_private(&path, &serde_json::to_vec_pretty(&saved)?)?;
            eprintln!("Logged in as {}. The password can go from relay.toml now.", config.user);
        }
    }
    Ok(client)
}

fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_message_carries_the_digest() {
        let digest = Digest {
            made: "2026-11-16T06:00:00Z".into(),
            text: "1. 2026-11-14 Bakery -45.00".into(),
            json: Some(serde_json::json!({"new": [{"n": 1, "id": "abc"}]})),
        };
        let mut conversation = Conversation { digest: Some(digest), session: None };
        let first = prompt(&conversation, "1 is eating out");
        assert!(first.contains("1 is eating out"));
        assert!(first.contains("Bakery"));
        assert!(first.contains("\"abc\""));
        conversation.session = Some("s".into());
        assert_eq!(prompt(&conversation, "and 2 later"), "and 2 later");
    }

    #[test]
    fn config_has_defaults() {
        let config: Config = toml::from_str(
            r#"
            homeserver = "https://matrix.example.org"
            user = "@bot:example.org"
            owner = "@me:example.org"
            room = "!room:example.org"
            clone = "/srv/konter"
            "#,
        )
        .unwrap();
        assert_eq!(config.daily, "just pinned more daily");
        assert_eq!(config.digest, "just pinned more konter digest");
        assert!(config.password.is_none());
        assert_eq!(config.wait_for, ["api.enablebanking.com:443"]);
    }

    #[test]
    fn waits_for_the_network() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap().to_string();
        assert!(wait_online(std::slice::from_ref(&open), Duration::ZERO));
        assert!(wait_online(&[], Duration::ZERO));
        drop(listener);
        // Nothing listens there now, and a zero limit gives up at once.
        assert!(!wait_online(&[open], Duration::ZERO));
        assert!(!wait_online(&["no-such-host.invalid:443".into()], Duration::ZERO));
    }

    #[test]
    fn keeps_the_last_lines() {
        assert_eq!(last_lines("a\nb\nc\n", 2), "b\nc");
        assert_eq!(last_lines("a", 5), "a");
    }
}
