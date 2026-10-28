use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 远程发布清单地址(本项目 GitHub Releases 的固定链接,公网匿名可访问)。
/// `releases/latest/download/` 永远 302 到最新正式版(非预发布)Release 的同名附件;
/// 发版工作流(.github/workflows/release.yml)会把 latest.json 作为附件上传,无需改代码。
const UPDATE_MANIFEST_URL: &str =
    "https://github.com/wenlingang/datum/releases/latest/download/latest.json";
const RELEASE_BASE_URL: &str = "https://github.com/wenlingang/datum/releases/download";

/// 发布清单(script/update-manifest.sh 生成)。只从可信的 HTTPS 地址取,
/// 但下载地址不信任清单:由版本号拼出,dmg 名也必须是 `Datum-<版本>.dmg`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// dmg 文件名,`Datum-<version>.dmg`。
    pub dmg: String,
    /// dmg 的 SHA-256(小写十六进制),下载后必须一致。
    pub sha256: String,
    /// dmg 字节数,用于下载进度;服务器给了 content-length 时以它为准。
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl Manifest {
    /// 解析并校验清单;版本号、文件名或摘要格式不对一律视为无效清单。
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let mut m: Manifest = serde_json::from_slice(bytes).ok()?;
        parse_version(&m.version)?;
        if m.dmg != format!("Datum-{}.dmg", m.version) {
            return None;
        }
        m.sha256 = m.sha256.to_ascii_lowercase();
        if m.sha256.len() != 64 || !m.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        m.size = m.size.filter(|&s| s > 0);
        m.notes = m
            .notes
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        Some(m)
    }

    /// dmg 下载地址,也是浏览器手动下载的地址。
    pub fn dmg_url(&self) -> String {
        format!("{RELEASE_BASE_URL}/v{}/{}", self.version, self.dmg)
    }
}

/// 清单地址。`update-override` 特性(仅截图/测试构建开启)下可用环境变量指向本地清单做端到端测试。
fn manifest_url() -> String {
    #[cfg(feature = "update-override")]
    if let Ok(url) = std::env::var("DATUM_UPDATE_MANIFEST_URL") {
        return url;
    }
    UPDATE_MANIFEST_URL.to_string()
}

/// 编译期版本号,离线即时可用,供状态栏显示当前版本。
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 拉取远程发布清单;任何失败(离线/超时/非 2xx/解析或校验失败)一律返回 None,静默不打扰用户。
pub async fn check_update() -> Option<Manifest> {
    let resp = reqwest::Client::new()
        .get(manifest_url())
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    Manifest::parse(&resp.bytes().await.ok()?)
}

/// exe 路径 → 所属 .app bundle:…/Datum.app/Contents/MacOS/datum 向上三级。
/// dev 运行(target/debug/datum)链条不以 .app 结尾 → None。
fn bundle_path_from_exe(exe: &Path) -> Option<PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension()? == "app").then(|| bundle.to_path_buf())
}

fn own_app_bundle() -> Option<PathBuf> {
    bundle_path_from_exe(&std::env::current_exe().ok()?)
}

/// 是否支持应用内自更新(打包为 .app 运行才支持);前端据此显示「开发版本,自动更新不可用」。
pub fn update_supported() -> bool {
    own_app_bundle().is_some()
}

/// 下载/安装进度,经 channel 推给界面。
#[derive(Clone, Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum ProgressEvent {
    /// pct = None 表示不知道总大小(进度不确定)。
    Downloading {
        pct: Option<u8>,
    },
    Installing,
}

/// 同一时刻只允许一个安装任务(界面状态机之外的兜底)。
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// 安装过程放在应用旁边的临时 bundle 名前缀(`.Datum-update-<pid>.app` 等)。
const LEFTOVER_PREFIX: &str = ".Datum-";

/// 应用内自更新:下载 dmg → 校验 SHA-256 → 挂载并拷出 .app → 用运行中应用的
/// designated requirement 校验新包签名(钉住发版证书)→ 核对版本 → 改名换入。
/// 成功后由用户点「重启」完成切换。
pub async fn download_and_install(
    manifest: Manifest,
    on_progress: impl Fn(ProgressEvent) + Send + Sync,
) -> Result<(), String> {
    if INSTALLING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(t!("core.update.in_progress").to_string());
    }
    let result = download_and_install_inner(&manifest, &on_progress).await;
    INSTALLING.store(false, Ordering::SeqCst);
    result
}

async fn download_and_install_inner(
    manifest: &Manifest,
    on_progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<(), String> {
    let app =
        own_app_bundle().ok_or_else(|| t!("core.update.dev_build_unsupported").to_string())?;
    if app
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return Err(t!("core.update.translocated").to_string());
    }
    // 先取要求再下载:ad-hoc 签名的应用(本机自打包)没有可钉住的证书,无从校验新包
    let requirement = designated_requirement(&app).await?;

    let work = std::env::temp_dir().join(format!("datum-update-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|e| t!("core.update.create_temp_dir_failed", error = e).to_string())?;
    let result = install_from(&app, &requirement, manifest, &work, on_progress).await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    result
}

async fn install_from(
    app: &Path,
    requirement: &str,
    manifest: &Manifest,
    work: &Path,
    on_progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<(), String> {
    let dmg = work.join(&manifest.dmg);
    download(&manifest.dmg_url(), &dmg, manifest.size, on_progress).await?;

    on_progress(ProgressEvent::Installing);
    if sha256_of(&dmg).await? != manifest.sha256 {
        return Err(t!("core.update.checksum_mismatch").to_string());
    }

    let parent = app
        .parent()
        .ok_or_else(|| t!("core.update.invalid_path", path = app.display()).to_string())?;
    remove_leftovers(parent).await;
    let pid = std::process::id();
    let staged = parent.join(format!("{LEFTOVER_PREFIX}update-{pid}.app"));
    let old = parent.join(format!("{LEFTOVER_PREFIX}old-{pid}.app"));

    let result = async {
        copy_out_of(&dmg, &work.join("mnt"), &staged).await?;
        run(
            "/usr/bin/codesign",
            &[
                "--verify",
                "--deep",
                "--strict",
                &format!("-R={requirement}"),
                path_str(&staged)?,
            ],
        )
        .await
        .map_err(|_| t!("core.update.codesign_invalid").to_string())?;
        let found = bundle_version(&staged).await.unwrap_or_default();
        if found != manifest.version {
            return Err(t!(
                "core.update.version_mismatch",
                found = found,
                expected = manifest.version
            )
            .to_string());
        }
        swap(app, &staged, &old).await
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&staged).await;
    result
}

/// 流式下载到 `to`,按百分比回报进度。
async fn download(
    url: &str,
    to: &Path,
    size: Option<u64>,
    on_progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt as _;

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30 * 60))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| t!("core.update.download_failed", error = e).to_string())?;
    if !resp.status().is_success() {
        return Err(t!(
            "core.update.download_failed",
            error = format!("HTTP {}", resp.status())
        )
        .to_string());
    }
    let total = resp.content_length().or(size).filter(|t| *t > 0);
    let mut file = tokio::fs::File::create(to)
        .await
        .map_err(|e| t!("core.update.create_file_failed", error = e).to_string())?;
    let mut downloaded: u64 = 0;
    let mut last_pct: Option<u8> = None;
    on_progress(ProgressEvent::Downloading { pct: None });
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| t!("core.update.download_interrupted", error = e).to_string())?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| t!("core.update.write_failed", error = e).to_string())?;
        downloaded += chunk.len() as u64;
        if let Some(t) = total {
            let pct = ((downloaded * 100 / t).min(100)) as u8;
            if last_pct != Some(pct) {
                last_pct = Some(pct);
                on_progress(ProgressEvent::Downloading { pct: Some(pct) });
            }
        }
    }
    file.flush()
        .await
        .map_err(|e| t!("core.update.write_failed", error = e).to_string())
}

async fn sha256_of(file: &Path) -> Result<String, String> {
    let out = tokio::process::Command::new("/usr/bin/shasum")
        .args(["-a", "256", path_str(file)?])
        .output()
        .await
        .map_err(|e| format!("shasum: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .next()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| t!("core.update.digest_failed").to_string())
}

/// 运行中应用的 designated requirement,如
/// `identifier "com.dbstudio.app" and certificate leaf = H"…"`。
async fn designated_requirement(app: &Path) -> Result<String, String> {
    let out = tokio::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r-", path_str(app)?])
        .output()
        .await
        .map_err(|e| format!("codesign: {e}"))?;
    // codesign 把要求打印在 stdout,其余信息在 stderr
    parse_requirement(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| t!("core.update.unsigned").to_string())
}

/// 取 `designated => …` 一行;不钉证书的要求(ad-hoc 签名只钉 cdhash,每次构建都变)拒绝。
fn parse_requirement(text: &str) -> Option<String> {
    let requirement = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("designated => "))?;
    requirement
        .contains("certificate ")
        .then(|| requirement.to_string())
}

/// 删掉上次被中途杀掉的安装留下的临时 bundle。
async fn remove_leftovers(parent: &Path) {
    let Ok(mut rd) = tokio::fs::read_dir(parent).await else {
        return;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(LEFTOVER_PREFIX) && name.ends_with(".app") {
            let _ = tokio::fs::remove_dir_all(entry.path()).await;
        }
    }
}

/// 只读挂载 dmg,用 ditto 拷出 Datum.app(保留符号链接与扩展属性),然后卸载。
async fn copy_out_of(dmg: &Path, mount: &Path, staged: &Path) -> Result<(), String> {
    tokio::fs::create_dir_all(mount)
        .await
        .map_err(|e| t!("core.update.create_temp_dir_failed", error = e).to_string())?;
    run(
        "/usr/bin/hdiutil",
        &[
            "attach",
            "-quiet",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
            path_str(mount)?,
            path_str(dmg)?,
        ],
    )
    .await?;
    let src = mount.join("Datum.app");
    let copied = if src.is_dir() {
        run("/usr/bin/ditto", &[path_str(&src)?, path_str(staged)?]).await
    } else {
        Err(t!("core.update.app_not_found").to_string())
    };
    let _ = run(
        "/usr/bin/hdiutil",
        &["detach", "-quiet", "-force", path_str(mount)?],
    )
    .await;
    copied
}

async fn bundle_version(app: &Path) -> Option<String> {
    let plist = app.join("Contents/Info.plist");
    let out = tokio::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString", plist.to_str()?])
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 运行中的应用改名挪开,新包改名到原位,再删旧包。运行中的进程不受影响:
/// 已打开的文件在改名后仍然有效。换入失败时把旧包挪回原位。
async fn swap(app: &Path, staged: &Path, old: &Path) -> Result<(), String> {
    let replace_error = |e: std::io::Error| {
        t!(
            "core.update.replace_failed",
            path = app.display(),
            error = e
        )
        .to_string()
    };
    tokio::fs::rename(app, old).await.map_err(replace_error)?;
    if let Err(e) = tokio::fs::rename(staged, app).await {
        let _ = tokio::fs::rename(old, app).await;
        return Err(replace_error(e));
    }
    let _ = tokio::fs::remove_dir_all(old).await;
    Ok(())
}

fn path_str(p: &Path) -> Result<&str, String> {
    p.to_str()
        .ok_or_else(|| t!("core.update.invalid_path", path = p.display()).to_string())
}

async fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = tokio::process::Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("{cmd}: {e}"))?;
    if !out.status.success() {
        return Err(t!(
            "core.update.command_failed",
            cmd = cmd,
            error = String::from_utf8_lossy(&out.stderr).trim()
        )
        .to_string());
    }
    Ok(())
}

/// `major.minor.patch`,可带 `v` 前缀;任一段不是数字即无效。
fn parse_version(v: &str) -> Option<[u64; 3]> {
    let v = v.trim();
    let v = v.strip_prefix(['v', 'V']).unwrap_or(v);
    let mut out = [0; 3];
    let mut parts = v.split('.');
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// latest 是否严格新于 current;任一版本号无效或相等/降级为 false。
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn manifest(version: &str, dmg: &str, sha: &str) -> Vec<u8> {
        format!(r#"{{"version":"{version}","dmg":"{dmg}","sha256":"{sha}","notes":" 修复 \n"}}"#)
            .into_bytes()
    }

    #[test]
    fn parse_manifest_builds_the_download_url() {
        let m = Manifest::parse(&manifest("0.2.0", "Datum-0.2.0.dmg", EMPTY_SHA)).unwrap();
        assert_eq!(m.version, "0.2.0");
        assert_eq!(
            m.dmg_url(),
            "https://github.com/wenlingang/datum/releases/download/v0.2.0/Datum-0.2.0.dmg"
        );
        assert_eq!(m.notes.as_deref(), Some("修复"));
        assert_eq!(m.size, None);
    }

    #[test]
    fn parse_manifest_reads_size_and_drops_empty_notes() {
        let json = format!(
            r#"{{"version":"0.2.0","dmg":"Datum-0.2.0.dmg","sha256":"{}","size":2048,"notes":"  "}}"#,
            EMPTY_SHA.to_uppercase()
        );
        let m = Manifest::parse(json.as_bytes()).unwrap();
        assert_eq!(m.size, Some(2048));
        assert_eq!(m.notes, None);
        assert_eq!(m.sha256, EMPTY_SHA);
    }

    #[test]
    fn parse_manifest_rejects_unexpected_names_and_checksums() {
        for (dmg, sha) in [
            ("../Datum-0.2.0.dmg", EMPTY_SHA),
            ("Datum-0.3.0.dmg", EMPTY_SHA),
            ("Datum-0.2.0.dmg", "abc"),
            ("Datum-0.2.0.dmg", &"z".repeat(64)),
        ] {
            assert!(
                Manifest::parse(&manifest("0.2.0", dmg, sha)).is_none(),
                "{dmg} {sha}"
            );
        }
        assert!(Manifest::parse(b"not json").is_none());
        assert!(Manifest::parse(&manifest("0.2", "Datum-0.2.dmg", EMPTY_SHA)).is_none());
    }

    #[test]
    fn parse_requirement_takes_the_designated_line() {
        let text = "# designated => identifier \"x\"\ndesignated => identifier \"com.dbstudio.app\" and certificate leaf = H\"ABC\"\n";
        assert_eq!(
            parse_requirement(text).as_deref(),
            Some(r#"identifier "com.dbstudio.app" and certificate leaf = H"ABC""#)
        );
    }

    #[test]
    fn parse_requirement_refuses_ad_hoc_signatures() {
        assert_eq!(parse_requirement("designated => cdhash H\"0123\"\n"), None);
        assert_eq!(
            parse_requirement("code object is not signed at all\n"),
            None
        );
    }

    #[test]
    fn app_version_matches_cargo() {
        assert_eq!(app_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn bundle_path_resolves_inside_app() {
        let p = Path::new("/Applications/Datum.app/Contents/MacOS/datum");
        assert_eq!(
            bundle_path_from_exe(p),
            Some(PathBuf::from("/Applications/Datum.app"))
        );
    }

    #[test]
    fn bundle_path_none_for_dev_builds() {
        assert_eq!(
            bundle_path_from_exe(Path::new("/x/target/debug/datum")),
            None
        );
        assert_eq!(bundle_path_from_exe(Path::new("/datum")), None);
    }

    #[test]
    fn progress_event_serializes_tagged() {
        let d = serde_json::to_value(ProgressEvent::Downloading { pct: Some(42) }).unwrap();
        assert_eq!(d["phase"], "downloading");
        assert_eq!(d["pct"], 42);
        let i = serde_json::to_value(ProgressEvent::Installing).unwrap();
        assert_eq!(i["phase"], "installing");
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.3.0", "0.2.13"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.1"));
        assert!(!is_newer("0.2.13", "0.2.13"));
        assert!(!is_newer("0.2.1", "0.2.10"));
        assert!(!is_newer("garbage", "0.0.1"));
        assert!(!is_newer("0.3.0-rc1", "0.2.0"));
    }

    #[tokio::test]
    async fn swap_replaces_the_bundle_and_removes_the_old_one() {
        let dir = std::env::temp_dir().join(format!("datum-swap-test-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let (app, staged, old) = (
            dir.join("Datum.app"),
            dir.join("new.app"),
            dir.join("old.app"),
        );
        tokio::fs::create_dir_all(&app).await.unwrap();
        tokio::fs::write(app.join("v"), "1").await.unwrap();
        tokio::fs::create_dir_all(&staged).await.unwrap();
        tokio::fs::write(staged.join("v"), "2").await.unwrap();

        swap(&app, &staged, &old).await.unwrap();
        assert_eq!(tokio::fs::read_to_string(app.join("v")).await.unwrap(), "2");
        assert!(!staged.exists() && !old.exists());

        // 新包不存在:换入失败,原应用挪回原位
        assert!(swap(&app, &staged, &old).await.is_err());
        assert_eq!(tokio::fs::read_to_string(app.join("v")).await.unwrap(), "2");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
