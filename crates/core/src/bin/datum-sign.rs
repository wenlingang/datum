//! 更新包签名工具(发版用)。
//!
//!   datum-sign keygen <私钥输出文件>   生成密钥对:私钥写入文件(权限 0600),公钥打印到标准输出
//!   datum-sign pubkey                  打印 $DATUM_UPDATE_SIGNING_KEY 对应的公钥
//!   datum-sign sign <文件>             用 $DATUM_UPDATE_SIGNING_KEY 签名,签名(十六进制)打印到标准输出
//!   datum-sign verify <公钥> <文件> <签名>
//!
//! 私钥是 64 位十六进制(32 字节种子),只放在发版环境(GitHub Secrets),不要提交进仓库。

use datum_core::update::{hex_encode, public_key_of, sign_bytes, verify_bytes};
use std::io::Read as _;
use std::process::ExitCode;

fn key_from_env() -> Result<String, String> {
    std::env::var("DATUM_UPDATE_SIGNING_KEY").map_err(|_| "未设置 DATUM_UPDATE_SIGNING_KEY".into())
}

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [cmd, out] if cmd == "keygen" => {
            let mut seed = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut seed))
                .map_err(|e| format!("读取随机数失败: {e}"))?;
            let sk = hex_encode(&seed);
            if std::path::Path::new(out).exists() {
                return Err(format!("{out} 已存在,不覆盖"));
            }
            std::fs::write(out, format!("{sk}\n")).map_err(|e| format!("写入失败: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("设置权限失败: {e}"))?;
            }
            println!("{}", public_key_of(&sk)?);
            Ok(())
        }
        [cmd] if cmd == "pubkey" => {
            println!("{}", public_key_of(&key_from_env()?)?);
            Ok(())
        }
        [cmd, file] if cmd == "sign" => {
            let data = std::fs::read(file).map_err(|e| format!("读取 {file} 失败: {e}"))?;
            println!("{}", sign_bytes(&key_from_env()?, &data)?);
            Ok(())
        }
        [cmd, pk, file, sig] if cmd == "verify" => {
            let data = std::fs::read(file).map_err(|e| format!("读取 {file} 失败: {e}"))?;
            if verify_bytes(pk, &data, sig) {
                println!("ok");
                Ok(())
            } else {
                Err("签名不符".into())
            }
        }
        _ => Err(
            "用法:datum-sign keygen <文件> | pubkey | sign <文件> | verify <公钥> <文件> <签名>"
                .into(),
        ),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
