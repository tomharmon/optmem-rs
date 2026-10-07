use std::{process::Stdio, time::Duration};

use optmem_agent::CallToolRequestParams;
use rmcp::{RoleClient, ServiceExt, service::RunningService};
use serde_json::{Value, json};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    time::timeout,
};

fn request(name: &str, args: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_owned()).with_arguments(args.as_object().unwrap().clone())
}

async fn connect(args: &[&str]) -> (RunningService<RoleClient, ()>, Child) {
    let isolation = args
        .windows(2)
        .find_map(|pair| match pair[0] {
            "--memory-dir" => Some(std::path::PathBuf::from(pair[1])),
            "--config" => Some(
                std::path::Path::new(pair[1])
                    .parent()
                    .unwrap()
                    .to_path_buf(),
            ),
            _ => None,
        })
        .expect("tests must select an isolated directory or config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_optmem-mcp"))
        .args(args)
        .env_remove("MEMORY_DIR")
        .env_remove("OPTMEM_CONFIG")
        .env("XDG_CONFIG_HOME", isolation.join(".test-config"))
        .env("XDG_DATA_HOME", isolation.join(".test-data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let transport = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let client = timeout(Duration::from_secs(5), ().serve(transport))
        .await
        .unwrap()
        .unwrap();
    (client, child)
}

async fn close(client: RunningService<RoleClient, ()>, mut child: Child) {
    client.cancel().await.unwrap();
    let status = timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .await
        .unwrap();
    assert!(status.success(), "{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected startup/protocol error: {stderr}"
    );
}

#[tokio::test]
async fn real_stdio_process_initializes_persists_and_reopens() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("an identity's memory");
    let path = dir.to_str().unwrap();
    let (client, child) = connect(&["--memory-dir", path, "--init"]).await;
    let list = client.list_tools(None).await.unwrap();
    assert_eq!(list.tools.len(), 5);
    let result = client
        .call_tool(request(
            "memory_note",
            json!({"text": "survives server restart"}),
        ))
        .await
        .unwrap();
    assert_eq!(
        result.structured_content.as_ref().unwrap()["result"]["memory"]["id"],
        0
    );
    close(client, child).await;
    assert_eq!(std::fs::metadata(dir.join("LOG.txt")).unwrap().len(), 320);
    let (client, child) = connect(&["--memory-dir", path]).await;
    let wake = client
        .call_tool(request("memory_wake", json!({})))
        .await
        .unwrap();
    assert_eq!(
        wake.structured_content.as_ref().unwrap()["result"]["entries"][0]["memory"]["text"],
        "survives server restart"
    );
    close(client, child).await;
}

#[tokio::test]
async fn missing_identity_and_bad_config_fail_without_protocol_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("missing");
    let result = Command::new(env!("CARGO_BIN_EXE_optmem-mcp"))
        .env_remove("OPTMEM_CONFIG")
        .env("XDG_CONFIG_HOME", temp.path().join(".test-config"))
        .env("XDG_DATA_HOME", temp.path().join(".test-data"))
        .arg("--memory-dir")
        .arg(&dir)
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--init"));
    assert!(!dir.exists());
    optmem::storage::FileStorage::initialize(&dir).unwrap();
    std::fs::write(dir.join("config"), "ENTRY_CHARS=999\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_optmem-mcp"))
        .env_remove("OPTMEM_CONFIG")
        .env("XDG_CONFIG_HOME", temp.path().join(".test-config"))
        .env("XDG_DATA_HOME", temp.path().join(".test-data"))
        .arg("--memory-dir")
        .arg(&dir)
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("config line 1"));
}

#[tokio::test]
async fn cli_read_only_and_forget_flags_apply_to_the_real_catalog() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().to_str().unwrap();
    let (client, child) = connect(&["--memory-dir", path, "--init", "--read-only"]).await;
    let list = client.list_tools(None).await.unwrap();
    assert_eq!(list.tools.len(), 4);
    assert!(!list.tools.iter().any(|t| t.name == "memory_note"));
    close(client, child).await;
    let (client, child) = connect(&["--memory-dir", path, "--allow-forget"]).await;
    assert!(
        client
            .list_tools(None)
            .await
            .unwrap()
            .tools
            .iter()
            .any(|t| t.name == "memory_forget")
    );
    close(client, child).await;
    let conflicting = Command::new(env!("CARGO_BIN_EXE_optmem-mcp"))
        .args(["--read-only", "--allow-forget"])
        .output()
        .await
        .unwrap();
    assert!(!conflicting.status.success());
}

#[cfg(unix)]
#[tokio::test]
async fn interrupt_exits_even_when_the_client_keeps_stdin_open() {
    let temp = tempfile::tempdir().unwrap();
    let (client, mut child) =
        connect(&["--memory-dir", temp.path().to_str().unwrap(), "--init"]).await;
    client.list_tools(None).await.unwrap();
    let pid = child.id().unwrap().to_string();
    assert!(
        Command::new("/bin/kill")
            .args(["-INT", &pid])
            .status()
            .await
            .unwrap()
            .success()
    );
    let status = timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn server_uses_user_config_storage_and_byte_limit_without_directory_arguments() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    let memory = temp.path().join("selected-memory");
    optmem::storage::FileStorage::initialize(&memory).unwrap();
    let settings = optmem::settings::AppSettings::load(Some(config.clone())).unwrap();
    settings
        .initialize(
            &memory,
            &optmem::Config {
                entry_chars: 12,
                ..optmem::Config::default()
            },
        )
        .unwrap();
    let (client, child) = connect(&["--config", config.to_str().unwrap()]).await;
    let denied = client
        .call_tool(request(
            "memory_note",
            json!({"text": "longer than twelve bytes"}),
        ))
        .await
        .unwrap();
    assert_eq!(denied.is_error, Some(true));
    let accepted = client
        .call_tool(request("memory_note", json!({"text": "short note"})))
        .await
        .unwrap();
    assert_eq!(accepted.is_error, Some(false));
    close(client, child).await;
    assert_eq!(
        std::fs::metadata(memory.join("LOG.txt")).unwrap().len(),
        320
    );
    assert!(!memory.join("config").exists());
}
