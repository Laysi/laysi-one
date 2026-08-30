//! `laysi-admin`: create local accounts, reset passwords, link external
//! identities.
//!
//! No clap: the four subcommands take fixed, minimal arguments, and
//! hand-written parsing keeps the deployment image and its dependencies
//! small. A temporary password is printed to stdout exactly once; every other
//! message goes to stderr so an administrator can pipe the password straight
//! into a password manager.

use anyhow::{anyhow, bail, Context};
use laysi_server::password;
use laysi_store::{CreateUser, Store};
use std::collections::HashMap;

const USAGE: &str = "\
Usage:
  laysi-admin create-user --username <u> --role <role> --display-name <n> [--employee-no <E001>] [--email <e>]
  laysi-admin reset-password --username <u>
  laysi-admin link-employee --username <u> --employee-no <E001>
  laysi-admin link-oauth --username <u> --provider <p> --subject <sub>
  laysi-admin list-users
  laysi-admin seed-demo [--force]        seed demo data (throwaway evaluation installs only)";

struct Cli {
    command: String,
    options: HashMap<String, String>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = parse_args(std::env::args().skip(1))?;
    if cli.command == "help" {
        println!("{USAGE}");
        return Ok(());
    }

    let database_url = laysi_store::env_or_file("DATABASE_URL")
        .unwrap_or_else(|| "postgres://postgres:postgres@localhost:5432/laysi".into());
    let store = Store::connect(&database_url, 2)
        .await
        .context("cannot connect to the database")?;
    store
        .migrate()
        .await
        .context("cannot apply database migrations")?;

    match cli.command.as_str() {
        "create-user" => create_user(&store, &cli).await,
        "reset-password" => reset_password(&store, &cli).await,
        "link-employee" => link_employee(&store, &cli).await,
        "link-oauth" => link_oauth(&store, &cli).await,
        "list-users" => list_users(&store).await,
        "seed-demo" => seed_demo(&store, &cli).await,
        _ => unreachable!("parse_args already rejected unknown subcommands"),
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> anyhow::Result<Cli> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        bail!("missing subcommand\n\n{USAGE}");
    };
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        return Ok(Cli {
            command: "help".into(),
            options: HashMap::new(),
        });
    }
    let allowed = allowed_options(&command)
        .ok_or_else(|| anyhow!("unsupported subcommand \"{command}\"\n\n{USAGE}"))?;
    let mut options = HashMap::new();
    while let Some(flag) = args.next() {
        if !flag.starts_with("--") {
            bail!("expected a flag, got \"{flag}\"\n\n{USAGE}");
        }
        if !allowed.contains(&flag.as_str()) {
            bail!("subcommand {command} takes no option \"{flag}\"\n\n{USAGE}");
        }
        let value = if valueless(&flag) {
            "true".to_string()
        } else {
            args.next()
                .ok_or_else(|| anyhow!("option {flag} has no value\n\n{USAGE}"))?
        };
        if options.insert(flag.clone(), value).is_some() {
            bail!("option {flag} given twice");
        }
    }
    Ok(Cli { command, options })
}

/// Flags that carry no value. Everything else takes the next argument, so
/// without this `--force` would swallow whatever followed it.
fn valueless(flag: &str) -> bool {
    flag == "--force"
}

fn allowed_options(command: &str) -> Option<&'static [&'static str]> {
    match command {
        "create-user" => Some(&[
            "--username",
            "--role",
            "--display-name",
            "--employee-no",
            "--email",
        ]),
        "reset-password" => Some(&["--username"]),
        "link-employee" => Some(&["--username", "--employee-no"]),
        "link-oauth" => Some(&["--username", "--provider", "--subject"]),
        "list-users" => Some(&[]),
        "seed-demo" => Some(&["--force"]),
        _ => None,
    }
}

fn required<'a>(cli: &'a Cli, flag: &str) -> anyhow::Result<&'a str> {
    cli.options
        .get(flag)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "subcommand {} is missing the required option {flag}\n\n{USAGE}",
                cli.command
            )
        })
}

async fn validate_role(store: &Store, code: &str) -> anyhow::Result<()> {
    let roles = store.roles().await.context("cannot read the role list")?;
    if roles.iter().any(|role| role.code == code) {
        return Ok(());
    }
    let legal = roles
        .iter()
        .map(|role| format!("{} ({})", role.code, role.name))
        .collect::<Vec<_>>()
        .join(", ");
    bail!("role \"{code}\" does not exist; valid values: {legal}");
}

async fn create_user(store: &Store, cli: &Cli) -> anyhow::Result<()> {
    let username = required(cli, "--username")?.trim();
    let role = required(cli, "--role")?.trim();
    let display_name = required(cli, "--display-name")?.trim();
    validate_role(store, role).await?;
    if store
        .user_by_username(username)
        .await
        .context("cannot check the username")?
        .is_some()
    {
        bail!("username \"{username}\" already exists");
    }

    let employee_id = match cli.options.get("--employee-no") {
        Some(employee_no) => Some(
            store
                .employee(employee_no.trim())
                .await
                .with_context(|| format!("no employee numbered \"{}\"", employee_no.trim()))?
                .id,
        ),
        None => None,
    };
    let email = cli.options.get("--email").map_or("", |value| value.trim());
    let temporary = password::temporary_password();
    let password_hash = password::hash_password(&temporary)
        .map_err(|error| anyhow!("cannot produce the Argon2id password hash: {error}"))?;
    let user = store
        .create_user(CreateUser {
            username,
            display_name,
            email,
            password_hash: Some(&password_hash),
            role_code: role,
            employee_id,
            status: "active",
            must_change_password: true,
            created_by: "laysi-admin",
        })
        .await
        .context("creating the user account failed")?;

    // stdout carries only the one-time secret, so the caller can pipe it
    // directly into a password manager.
    println!("{temporary}");
    eprintln!(
        "created account {} ({}, role {}); the password must be changed at the first sign-in.",
        user.username, user.display_name, user.role_code
    );
    Ok(())
}

async fn reset_password(store: &Store, cli: &Cli) -> anyhow::Result<()> {
    let username = required(cli, "--username")?.trim();
    let user = store
        .user_by_username(username)
        .await
        .context("looking up the user account failed")?
        .ok_or_else(|| anyhow!("no user account named \"{username}\""))?;
    let temporary = password::temporary_password();
    let password_hash = password::hash_password(&temporary)
        .map_err(|error| anyhow!("cannot produce the Argon2id password hash: {error}"))?;
    store
        .set_password_hash(user.id, Some(&password_hash), true)
        .await
        .context("resetting the password failed")?;

    println!("{temporary}");
    eprintln!("password reset for {username}; it must be changed at the next sign-in.");
    Ok(())
}

/// Point an existing login at a roster row.
///
/// `create-user --employee-no` only covers accounts made after the person was
/// on the roster. The owner needs this too: an owner is an employee, and
/// without the link the self-service screens have nobody to be about.
async fn link_employee(store: &Store, cli: &Cli) -> anyhow::Result<()> {
    let username = required(cli, "--username")?.trim();
    let employee_no = required(cli, "--employee-no")?.trim();
    let user = store
        .user_by_username(username)
        .await
        .context("looking up the user account failed")?
        .ok_or_else(|| anyhow!("no user account named \"{username}\""))?;
    let employee = store
        .employee(employee_no)
        .await
        .with_context(|| format!("no employee numbered \"{employee_no}\""))?;
    store
        .set_employee_link(user.id, Some(employee.id))
        .await
        .context("linking the employee record failed")?;
    eprintln!(
        "linked account {username} to employee {employee_no} ({}).",
        employee.name
    );
    Ok(())
}

fn normalize_provider(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

async fn link_oauth(store: &Store, cli: &Cli) -> anyhow::Result<()> {
    let username = required(cli, "--username")?.trim();
    let provider = normalize_provider(required(cli, "--provider")?);
    if provider.is_empty() {
        bail!("--provider must not be empty");
    }
    let subject = required(cli, "--subject")?;
    let user = store
        .user_by_username(username)
        .await
        .context("looking up the user account failed")?
        .ok_or_else(|| anyhow!("no user account named \"{username}\""))?;
    store
        .link_identity(user.id, &provider, subject, &user.email)
        .await
        .context("linking the OAuth identity failed; check that the provider and subject are not already used by another account")?;
    eprintln!("linked {provider} subject \"{subject}\" to account {username}.");
    Ok(())
}

/// Demo data, then a staff login to read it with.
///
/// The seeding itself refuses to touch an installation that already has
/// vouchers unless `--force` is given; see `laysi_server::demo`.
async fn seed_demo(store: &Store, cli: &Cli) -> anyhow::Result<()> {
    let force = cli.options.contains_key("--force");
    let today = std::env::var("LAYSI_TODAY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| chrono::Utc::now().date_naive());

    let calls = laysi_server::demo::seed(store, today, force).await?;

    // The owner is an employee too. Any existing owner login is pointed at
    // the roster's own manager row, so 我的 is not missing for the account
    // most likely to be looking at it.
    if let Some(owner) = store
        .users()
        .await
        .context("reading the user list failed")?
        .into_iter()
        .find(|u| u.role_code == "boss" && u.employee_id.is_none())
    {
        let manager = store
            .employee(laysi_server::demo::OWNER_EMPLOYEE_NO)
            .await
            .context("the demo owner employee record is missing")?;
        store
            .set_employee_link(owner.id, Some(manager.id))
            .await
            .context("linking the owner account to its employee record failed")?;
        eprintln!(
            "linked owner account {} to employee {} ({}).",
            owner.username,
            laysi_server::demo::OWNER_EMPLOYEE_NO,
            manager.name
        );
    }

    // A staff login for the self-service screens. Without one those four
    // screens can only show "this account is not linked to an employee",
    // which is the least interesting thing they do.
    let username = "demo";
    let temporary = password::temporary_password();
    if store
        .user_by_username(username)
        .await
        .context("cannot check the demo account")?
        .is_none()
    {
        let employee = store
            .employee(laysi_server::demo::STAFF_EMPLOYEE_NO)
            .await
            .context("the demo employee is missing; the demo data may be incomplete")?;
        let password_hash = password::hash_password(&temporary)
            .map_err(|error| anyhow!("cannot produce the Argon2id password hash: {error}"))?;
        store
            .create_user(CreateUser {
                username,
                display_name: &employee.name,
                email: "",
                password_hash: Some(&password_hash),
                role_code: "staff",
                employee_id: Some(employee.id),
                status: "active",
                must_change_password: true,
                created_by: "laysi-admin",
            })
            .await
            .context("creating the demo employee account failed")?;
        println!("{temporary}");
        eprintln!(
            "created self-service demo account {username} ({}); the password must be changed at the first sign-in.",
            employee.name
        );
    } else {
        eprintln!("account {username} already exists and was left alone; use reset-password for a new one.");
    }

    eprintln!("demo data seeded: {calls} operations, all through Registry::dispatch, with a complete audit trail.");
    eprintln!("this is evaluation data for throwaway installs; do not seed it onto real books.");
    Ok(())
}

fn cell(value: &str) -> String {
    value.replace(['\t', '\r', '\n'], " ")
}

async fn list_users(store: &Store) -> anyhow::Result<()> {
    let users = store
        .users()
        .await
        .context("reading the user list failed")?;
    println!("username\trole\tstatus\tmust_change_password\temployee_id\tdisplay_name\temail");
    for user in users {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            cell(&user.username),
            cell(&user.role_code),
            cell(&user.status),
            if user.must_change_password {
                "yes"
            } else {
                "no"
            },
            user.employee_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            cell(&user.display_name),
            cell(&user.email),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings<'a>(values: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        values.iter().map(|value| (*value).to_string())
    }

    #[test]
    fn parser_accepts_the_documented_create_user_shape() {
        let cli = parse_args(strings(&[
            "create-user",
            "--username",
            "owner",
            "--role",
            "boss",
            "--display-name",
            "負責人",
            "--employee-no",
            "E001",
        ]))
        .expect("the documented create-user shape should parse");
        assert_eq!(cli.command, "create-user");
        assert_eq!(
            required(&cli, "--username").expect("username should be present"),
            "owner"
        );
        assert_eq!(
            required(&cli, "--role").expect("role should be present"),
            "boss"
        );
    }

    #[test]
    fn parser_rejects_unknown_or_duplicate_flags() {
        assert!(parse_args(strings(&["reset-password", "--role", "boss"])).is_err());
        assert!(parse_args(strings(&[
            "reset-password",
            "--username",
            "owner",
            "--username",
            "again",
        ]))
        .is_err());
    }

    /// `--force` carries no value, so it must not eat the argument after it
    /// and must still be rejected on subcommands that do not take it.
    #[test]
    fn force_is_a_valueless_flag() {
        let cli =
            parse_args(strings(&["seed-demo", "--force"])).expect("seed-demo --force should parse");
        assert!(cli.options.contains_key("--force"));
        assert!(parse_args(strings(&["list-users", "--force"])).is_err());
    }

    #[test]
    fn oidc_provider_matches_runtime_normalization() {
        assert_eq!(
            normalize_provider("https://login.example.test/tenant/"),
            "https://login.example.test/tenant"
        );
        assert_eq!(normalize_provider("google"), "google");
    }

    #[test]
    fn list_cells_cannot_inject_extra_rows_or_columns() {
        assert_eq!(cell("一\t二\n三"), "一 二 三");
    }
}
