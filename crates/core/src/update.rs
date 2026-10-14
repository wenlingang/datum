use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 远程发布清单地址(本项目 GitHub Releases 的固定链接,公网匿名可访问)。
/// `releases/latest/download/` 永远 302 到最新 Release 的同名附件;
/// 发版工作流(.github/workflows/release.yml)会把 latest.json 作为附件上传,无需改代码。
const UPDATE_MANIFEST_URL: &str =
    "https://github.com/wenlingang/datum/releases/latest/download/latest.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// dmg 下载链接(浏览器打开,老版本 App 的兼容路径)。
    pub url: String,
    /// 应用内自更新的 zip 包(签名 + 公证 + staple 后的 .app 打包);缺省时前端回退浏览器下载。
    #[serde(default)]
    pub zip_url: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// zip 包的 SHA-256(十六进制);有则下载后校验,不一致拒绝安装。
    #[serde(default)]
    pub sha256: Option<String>,
    /// zip 包的 Ed25519 签名(十六进制)。安装前用内置公钥验签,缺失或不符一律拒绝。
    #[serde(default)]
    pub signature: Option<String>,
}

/// 更新包签名公钥(Ed25519,十六进制)。对应私钥只存在发版环境(GitHub Secrets),
/// 用 `datum-sign keygen` 生成;换密钥需同时替换这里并发一版过渡。
pub const UPDATE_PUBLIC_KEY: &str =
    "f7398b8c91175e124e8de5c254539074dc183991d2a410c6d33f5b1ccf13291d";

fn hex_decode<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 由私钥(32 字节种子,十六进制)对数据签名,返回十六进制签名。发版工具用。
pub fn sign_bytes(private_key_hex: &str, data: &[u8]) -> Result<String, String> {
    use ed25519_dalek::Signer as _;
    let seed = hex_decode::<32>(private_key_hex)
        .ok_or_else(|| t!("core.update.invalid_private_key").to_string())?;
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    Ok(hex_encode(&key.sign(data).to_bytes()))
}

/// 私钥对应的公钥(十六进制)。
pub fn public_key_of(private_key_hex: &str) -> Result<String, String> {
    let seed = hex_decode::<32>(private_key_hex)
        .ok_or_else(|| t!("core.update.invalid_private_key").to_string())?;
    Ok(hex_encode(
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .as_bytes(),
    ))
}

/// 用公钥验证数据的签名。
pub fn verify_bytes(public_key_hex: &str, data: &[u8], signature_hex: &str) -> bool {
    let (Some(pk), Some(sig)) = (
        hex_decode::<32>(public_key_hex),
        hex_decode::<64>(signature_hex),
    ) else {
        return false;
    };
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    key.verify_strict(data, &ed25519_dalek::Signature::from_bytes(&sig))
        .is_ok()
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

/// 拉取远程发布清单;任何失败(离线/超时/非 2xx/解析错)一律返回 None,静默不打扰用户。
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
    resp.json::<Manifest>().await.ok()
}

/// exe 路径 → 所属 .app bundle:…/Datum.app/Contents/MacOS/dbstudio 向上三级。
/// dev 运行(target/debug/dbstudio)链条不以 .app 结尾 → None。
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

/// 下载/安装进度,经 ipc Channel 推给前端。
#[derive(Clone, Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum ProgressEvent {
    /// pct = None 表示服务器没给 content-length(进度不确定)。
    Downloading {
        pct: Option<u8>,
    },
    Installing,
}

/// 同一时刻只允许一个安装任务(前端状态机之外的兜底)。
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// 应用内自更新:流式下载 zip → 校验摘要 → ditto 解压 → 校验签名 → rsync 原地替换运行中的 .app。
/// 成功后由用户点「重启」调 restart_app 完成切换。
pub async fn download_and_install(
    url: String,
    sha256: Option<String>,
    signature: Option<String>,
    on_progress: impl Fn(ProgressEvent) + Send + Sync,
) -> Result<(), String> {
    if INSTALLING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(t!("core.update.in_progress").to_string());
    }
    let result =
        download_and_install_inner(&url, sha256.as_deref(), signature.as_deref(), &on_progress)
            .await;
    INSTALLING.store(false, Ordering::SeqCst);
    result
}

async fn download_and_install_inner(
    url: &str,
    sha256: Option<&str>,
    signature: Option<&str>,
    on_progress: &(dyn Fn(ProgressEvent) + Send + Sync),
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt as _;

    let app_path =
        own_app_bundle().ok_or_else(|| t!("core.update.dev_build_unsupported").to_string())?;
    if app_path.to_string_lossy().contains("AppTranslocation") {
        return Err(t!("core.update.translocated").to_string());
    }

    let work = std::env::temp_dir().join(format!("datum-update-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|e| t!("core.update.create_temp_dir_failed", error = e).to_string())?;

    // 流式下载 zip,仅在整数百分比变化时上报,避免刷爆 IPC。
    let zip_path = work.join("update.zip");
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
    let total = resp.content_length();
    let mut file = tokio::fs::File::create(&zip_path)
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
        if let Some(t) = total.filter(|t| *t > 0) {
            let pct = ((downloaded * 100 / t).min(100)) as u8;
            if last_pct != Some(pct) {
                last_pct = Some(pct);
                on_progress(ProgressEvent::Downloading { pct: Some(pct) });
            }
        }
    }
    file.flush()
        .await
        .map_err(|e| t!("core.update.write_failed", error = e).to_string())?;
    drop(file);

    on_progress(ProgressEvent::Installing);

    if let Some(expected) = sha256.filter(|h| !h.trim().is_empty()) {
        let actual = sha256_of(&zip_path).await?;
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            let _ = tokio::fs::remove_dir_all(&work).await;
            return Err(t!("core.update.checksum_mismatch").to_string());
        }
    }
    // 签名必须有且验证通过:摘要与清单同源,只能防传输损坏;签名才能证明出自发版私钥
    let data = tokio::fs::read(&zip_path)
        .await
        .map_err(|e| t!("core.update.read_package_failed", error = e).to_string())?;
    if !signature.is_some_and(|sig| verify_bytes(UPDATE_PUBLIC_KEY, &data, sig)) {
        let _ = tokio::fs::remove_dir_all(&work).await;
        return Err(t!("core.update.signature_failed").to_string());
    }
    drop(data);

    // ditto 解压:正确处理 .app 的符号链接与扩展属性(勿换 zip 库)。
    let out = work.join("out");
    tokio::fs::create_dir_all(&out)
        .await
        .map_err(|e| t!("core.update.create_extract_dir_failed", error = e).to_string())?;
    run(
        "ditto",
        &["-x", "-k", path_str(&zip_path)?, path_str(&out)?],
    )
    .await?;

    let new_app = find_dot_app(&out)
        .await
        .ok_or_else(|| t!("core.update.app_not_found").to_string())?;
    if let Err(e) = verify_same_publisher(&app_path, &new_app).await {
        let _ = tokio::fs::remove_dir_all(&work).await;
        return Err(e);
    }

    // 原地替换:两端尾斜杠 = 同步目录内容;--delete 清除新版删掉的文件。
    // rsync 是最后一步,此前任何失败都不会碰运行中的 .app。
    let mut src = path_str(&new_app)?.to_string();
    src.push('/');
    let mut dst = path_str(&app_path)?.to_string();
    dst.push('/');
    run("rsync", &["-a", "--delete", &src, &dst]).await?;

    let _ = tokio::fs::remove_dir_all(&work).await;
    Ok(())
}

async fn sha256_of(file: &Path) -> Result<String, String> {
    let out = tokio::process::Command::new("shasum")
        .args(["-a", "256", path_str(file)?])
        .output()
        .await
        .map_err(|e| format!("shasum: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| t!("core.update.digest_failed").to_string())
}

/// `codesign -dv` 输出里的 TeamIdentifier;未签名、ad-hoc 签名或未设置时为 None。
async fn team_id(app: &Path) -> Option<String> {
    let out = tokio::process::Command::new("codesign")
        .args(["-dv", "--verbose=2", app.to_str()?])
        .output()
        .await
        .ok()?;
    parse_team_id(&String::from_utf8_lossy(&out.stderr))
}

fn parse_team_id(codesign_output: &str) -> Option<String> {
    codesign_output
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "not set")
        .map(str::to_string)
}

async fn bundle_id(app: &Path) -> Option<String> {
    let plist = app.join("Contents/Info.plist");
    let out = tokio::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleIdentifier", plist.to_str()?])
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 新包必须与正在运行的应用同一标识符;当前应用有开发者签名时,新包也必须签名完整有效、
/// 且属于同一开发者团队。本机自打的未签名包(无团队)只校验标识符。
async fn verify_same_publisher(current: &Path, new: &Path) -> Result<(), String> {
    if bundle_id(current).await != bundle_id(new).await {
        return Err(t!("core.update.bundle_id_mismatch").to_string());
    }
    let Some(team) = team_id(current).await else {
        return Ok(());
    };
    run(
        "codesign",
        &["--verify", "--deep", "--strict", path_str(new)?],
    )
    .await
    .map_err(|_| t!("core.update.codesign_invalid").to_string())?;
    if team_id(new).await.as_deref() != Some(team.as_str()) {
        return Err(t!("core.update.different_developer").to_string());
    }
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

async fn find_dot_app(dir: &Path) -> Option<PathBuf> {
    let mut rd = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "app") {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_deserializes_with_and_without_notes() {
        let m: Manifest =
            serde_json::from_str(r#"{"version":"0.2.0","url":"https://x/a.dmg","notes":"n"}"#)
                .unwrap();
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.url, "https://x/a.dmg");
        assert_eq!(m.notes.as_deref(), Some("n"));
        assert_eq!(m.zip_url, None);

        let m2: Manifest =
            serde_json::from_str(r#"{"version":"0.2.0","url":"https://x/a.dmg"}"#).unwrap();
        assert_eq!(m2.notes, None);
    }

    #[test]
    fn manifest_deserializes_with_zip_url() {
        let m: Manifest = serde_json::from_str(
            r#"{"version":"0.3.0","url":"https://x/a.dmg","zip_url":"https://x/a.app.zip"}"#,
        )
        .unwrap();
        assert_eq!(m.zip_url.as_deref(), Some("https://x/a.app.zip"));
    }

    #[test]
    fn manifest_sha256_optional() {
        let m: Manifest =
            serde_json::from_str(r#"{"version":"0.3.0","url":"u","zip_url":"z","sha256":"abc"}"#)
                .unwrap();
        assert_eq!(m.sha256.as_deref(), Some("abc"));
    }

    #[test]
    fn signs_and_verifies() {
        let sk = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
        let pk = public_key_of(sk).unwrap();
        // RFC 8032 测试向量 1
        assert_eq!(
            pk,
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        let sig = sign_bytes(sk, b"zip bytes").unwrap();
        assert!(verify_bytes(&pk, b"zip bytes", &sig));
        assert!(!verify_bytes(&pk, b"zip bytez", &sig));
        assert!(!verify_bytes(&pk, b"zip bytes", &sig.replace('a', "b")));
        assert!(!verify_bytes(&pk, b"zip bytes", "nothex"));
        assert!(sign_bytes("short", b"x").is_err());
    }

    #[test]
    fn builtin_public_key_is_valid() {
        assert!(hex_decode::<32>(UPDATE_PUBLIC_KEY)
            .and_then(|k| ed25519_dalek::VerifyingKey::from_bytes(&k).ok())
            .is_some());
    }

    #[test]
    fn parses_team_identifier() {
        let signed = "Executable=/A.app/Contents/MacOS/datum\nAuthority=Developer ID Application: X (TEAM123)\nTeamIdentifier=TEAM123\n";
        assert_eq!(parse_team_id(signed).as_deref(), Some("TEAM123"));
        assert_eq!(
            parse_team_id("Signature=adhoc\nTeamIdentifier=not set\n"),
            None
        );
        assert_eq!(parse_team_id("code object is not signed at all"), None);
    }

    #[test]
    fn app_version_matches_cargo() {
        assert_eq!(app_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn bundle_path_resolves_inside_app() {
        let p = Path::new("/Applications/Datum.app/Contents/MacOS/dbstudio");
        assert_eq!(
            bundle_path_from_exe(p),
            Some(PathBuf::from("/Applications/Datum.app"))
        );
    }

    #[test]
    fn bundle_path_none_for_dev_builds() {
        assert_eq!(
            bundle_path_from_exe(Path::new("/x/target/debug/dbstudio")),
            None
        );
        assert_eq!(bundle_path_from_exe(Path::new("/dbstudio")), None);
    }

    #[test]
    fn progress_event_serializes_tagged() {
        let d = serde_json::to_value(ProgressEvent::Downloading { pct: Some(42) }).unwrap();
        assert_eq!(d["phase"], "downloading");
        assert_eq!(d["pct"], 42);
        let i = serde_json::to_value(ProgressEvent::Installing).unwrap();
        assert_eq!(i["phase"], "installing");
    }
}

/// latest 是否严格新于 current(`major.minor.patch`,缺段或非数字按 0);相等/降级为 false。
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> [u64; 3] {
        let mut out = [0; 3];
        let v = v.trim();
        let v = v.strip_prefix(['v', 'V']).unwrap_or(v);
        for (i, part) in v.split('.').take(3).enumerate() {
            out[i] = part.parse().unwrap_or(0);
        }
        out
    }
    parse(latest) > parse(current)
}

#[cfg(test)]
mod semver_tests {
    use super::is_newer;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.3.0", "0.2.13"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.13", "0.2.13"));
        assert!(!is_newer("0.2.1", "0.2.10"));
        assert!(!is_newer("garbage", "0.0.1"));
    }
}
