use super::*;
use base64::Engine;
use std::process::Stdio;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandRequest {
    pub script: String,
    #[serde(default)]
    pub working_directory: String,
    #[serde(default)]
    pub shell: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}
fn default_timeout() -> u64 {
    60
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandReport {
    pub request: CommandRequest,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub truncated: bool,
}

pub(super) fn working_directory(value: &str) -> Result<PathBuf> {
    let path = if value.trim().is_empty() {
        directories::BaseDirs::new()
            .context("无法确定用户目录")?
            .home_dir()
            .to_owned()
    } else {
        PathBuf::from(value.trim())
    };
    ensure!(
        path.is_absolute() && path.is_dir(),
        "命令工作目录必须是已存在的绝对目录"
    );
    Ok(path)
}

pub(super) fn tool(directory: &str, events: Sender<ChatEvent>) -> Result<DynamicTool> {
    let directory = working_directory(directory)?;
    Ok(DynamicTool::new(
        ToolName::new("execute_command")?,
        format!(
            "Execute a local command/script and return stdout, stderr, exit code and timeout status. Default cwd: {}. Default shell: {}. PowerShell supports multiline scripts and invoking .ps1 files with &. Each call is a new non-interactive process. Use actual results to continue the task. Commands run with the user's ordinary permissions; no elevation. Timeout 1-120 seconds, output capped at 64KiB per stream.",
            directory.display(),
            if cfg!(windows) { "powershell" } else { "sh" }
        ),
        serde_json::json!({"type":"object","properties":{
            "script":{"type":"string","description":"Command or multiline script to execute"},
            "working_directory":{"type":"string","description":"Optional absolute existing cwd; empty uses configured directory"},
            "shell":{"type":"string","enum":["powershell","sh"],"description":"Optional; powershell on Windows, sh on macOS by default"},
            "timeout_seconds":{"type":"integer","minimum":1,"maximum":120}
        },"required":["script"],"additionalProperties":false}),
        move |args| {
            let directory = directory.clone();
            let events = events.clone();
            Box::pin(async move {
                let mut request: CommandRequest = serde_json::from_value(args)
                    .map_err(|e| ToolExecutionError::invalid_args(e.to_string()))?;
                if request.working_directory.trim().is_empty() {
                    request.working_directory = directory.to_string_lossy().into_owned();
                }
                validate(&request).map_err(|e| ToolExecutionError::invalid_args(e.to_string()))?;
                let _ = events
                    .send(ChatEvent::CommandStarted(request.clone()))
                    .await;
                let report = match execute(request.clone()).await {
                    Ok(report) => report,
                    Err(error) => CommandReport {
                        request,
                        success: false,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: format!("{error:#}"),
                        timed_out: false,
                        truncated: false,
                    },
                };
                let _ = events
                    .send(ChatEvent::CommandFinished(report.clone()))
                    .await;
                Ok(ToolOutput::json(serde_json::to_value(report).unwrap()))
            })
        },
    ))
}

fn validate(request: &CommandRequest) -> Result<()> {
    ensure!(
        !request.script.trim().is_empty()
            && request.script.len() <= 32_000
            && !request.script.contains('\0'),
        "命令须为 1 至 32000 字节，不能包含 NUL"
    );
    ensure!(
        (1..=120).contains(&request.timeout_seconds),
        "命令超时须为 1 至 120 秒"
    );
    ensure!(
        matches!(request.shell.as_str(), "" | "powershell" | "sh"),
        "不支持的命令解释器"
    );
    working_directory(&request.working_directory)?;
    Ok(())
}

// Drain both pipes concurrently even after the retention cap; a noisy child
// cannot block on a full pipe or allocate unlimited memory.
async fn read_output(mut pipe: impl AsyncRead + Unpin) -> std::io::Result<(String, bool)> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = pipe.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        let take = n.min(65536usize.saturating_sub(kept.len()));
        kept.extend_from_slice(&buffer[..take]);
        truncated |= take < n;
    }
    Ok((String::from_utf8_lossy(&kept).into_owned(), truncated))
}

async fn execute(mut request: CommandRequest) -> Result<CommandReport> {
    validate(&request)?;
    let cwd = working_directory(&request.working_directory)?;
    if request.shell.is_empty() {
        request.shell = if cfg!(windows) { "powershell" } else { "sh" }.into();
    }
    let mut command;
    if request.shell == "powershell" {
        #[cfg(windows)]
        let executable =
            PathBuf::from(std::env::var_os("SystemRoot").context("找不到 Windows 系统目录")?)
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        #[cfg(not(windows))]
        let executable = PathBuf::from("pwsh");
        command = Command::new(executable);
        let script = format!(
            "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding; $ErrorActionPreference = 'Stop'; $global:LASTEXITCODE = 0; try {{\n{}\nexit $LASTEXITCODE\n}} catch {{ [Console]::Error.WriteLine(($_ | Out-String)); exit 1 }}",
            request.script
        );
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
            ])
            .arg(base64::engine::general_purpose::STANDARD.encode(bytes));
    } else {
        command = Command::new("/bin/sh");
        command.arg("-c").arg(&request.script);
    }
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().context("无法启动命令解释器")?;
    let group = ProcessGroup::attach(&child)?;
    let stdout = child.stdout.take().context("缺少标准输出管道")?;
    let stderr = child.stderr.take().context("缺少标准错误管道")?;
    let result = tokio::time::timeout(Duration::from_secs(request.timeout_seconds), async {
        tokio::try_join!(child.wait(), read_output(stdout), read_output(stderr))
    })
    .await;
    // Closing this guard terminates descendants, including on cancellation or
    // timeout when Rig drops the tool future. No background shell is retained.
    drop(group);
    match result {
        Ok(result) => {
            let (status, (stdout, out_cut), (stderr, err_cut)) = result?;
            Ok(CommandReport {
                request,
                success: status.success(),
                exit_code: status.code(),
                stdout,
                stderr,
                timed_out: false,
                truncated: out_cut || err_cut,
            })
        }
        Err(_) => {
            let _ = child.kill().await;
            Ok(CommandReport {
                request,
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: "命令执行超时，进程已结束".into(),
                timed_out: true,
                truncated: false,
            })
        }
    }
}

#[cfg(windows)]
struct ProcessGroup(windows::Win32::Foundation::HANDLE);
#[cfg(windows)]
unsafe impl Send for ProcessGroup {}
#[cfg(windows)]
impl ProcessGroup {
    fn attach(child: &tokio::process::Child) -> Result<Self> {
        use windows::{
            Win32::{Foundation::HANDLE, System::JobObjects::*},
            core::PCWSTR,
        };
        unsafe {
            let group = Self(CreateJobObjectW(None, PCWSTR::null())?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                group.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                std::mem::size_of_val(&limits) as u32,
            )?;
            AssignProcessToJobObject(
                group.0,
                HANDLE(child.raw_handle().context("命令进程无效")? as _),
            )?;
            Ok(group)
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(unix)]
struct ProcessGroup(i32);
#[cfg(unix)]
impl ProcessGroup {
    fn attach(child: &tokio::process::Child) -> Result<Self> {
        Ok(Self(child.id().context("命令进程无效")? as i32))
    }
}
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[tokio::test]
    async fn dropping_execution_terminates_spawned_descendants() {
        use windows::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-pid.txt");
        let script = format!(
            "$p = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -WindowStyle Hidden -PassThru -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 30'; Set-Content -LiteralPath '{}' -Value $p.Id; Start-Sleep -Seconds 30",
            marker.display().to_string().replace('\'', "''")
        );
        let running = tokio::spawn(execute(request(&script)));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while !marker.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let pid: u32 = fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        let process =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).unwrap() };
        running.abort();
        let _ = running.await;
        let mut exit = 259;
        for _ in 0..30 {
            unsafe {
                GetExitCodeProcess(process, &mut exit).unwrap();
            }
            if exit != 259 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        unsafe {
            let _ = CloseHandle(process);
        }
        assert_ne!(exit, 259, "cancelled command left a descendant running");
    }
    fn request(script: &str) -> CommandRequest {
        CommandRequest {
            script: script.into(),
            working_directory: String::new(),
            shell: String::new(),
            timeout_seconds: 10,
        }
    }
    #[tokio::test]
    async fn runs_unicode_script_with_working_directory_and_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut req = request(if cfg!(windows) {
            "'你好'; [Console]::Error.WriteLine('错误'); (Get-Location).Path; exit 7"
        } else {
            "printf '你好'; printf '错误' >&2; pwd; exit 7"
        });
        req.working_directory = dir.path().to_string_lossy().into_owned();
        let output = execute(req).await.unwrap();
        assert!(!output.success);
        assert_eq!(output.exit_code, Some(7));
        assert!(output.stdout.contains("你好"));
        assert!(output.stderr.contains("错误"));
        assert!(
            output.stdout.to_lowercase().contains(
                &dir.path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_lowercase()
            )
        );
    }
    #[tokio::test]
    async fn command_timeout_and_invalid_arguments_are_bounded() {
        let mut req = request(if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        });
        req.timeout_seconds = 1;
        let start = std::time::Instant::now();
        assert!(execute(req).await.unwrap().timed_out);
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(validate(&request("")).is_err());
    }
    #[tokio::test]
    async fn drains_both_streams_and_truncates_output() {
        let out = execute(request(if cfg!(windows) {
            "[Console]::Out.Write(('x' * 100000)); [Console]::Error.Write(('e' * 100000))"
        } else {
            "head -c 100000 /dev/zero; head -c 100000 /dev/zero >&2"
        }))
        .await
        .unwrap();
        assert!(out.success && out.truncated);
        assert_eq!(out.stdout.len(), 65536);
        assert_eq!(out.stderr.len(), 65536);
    }
}
