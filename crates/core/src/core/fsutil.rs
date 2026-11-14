//! 配置目录里的私有文件(连接、历史、AI 设置、会话……)的写入。

use std::io::Write;
use std::path::Path;

/// 写入只有本用户能读写的文件:临时文件创建时就是 0600(不存在先以默认权限写入再收紧的空档),
/// 写完再改名替换原文件(写到一半崩溃不会留下半截文件)。父目录不存在时创建(0700)。
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            let mut b = std::fs::DirBuilder::new();
            b.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            b.create(parent)?;
        }
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    // 残留的同名临时文件可能是别的权限:先删掉,保证下面是新建
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_file_with_private_permissions() {
        let dir = std::env::temp_dir().join(format!("datum-fsutil-{}", uuid::Uuid::new_v4()));
        let path = dir.join("sub").join("a.json");
        write_private(&path, b"one").unwrap();
        // 原文件权限宽松也会被替换成 0600 的新文件
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_private(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(dir.join("sub")).unwrap().permissions().mode() & 0o777, 0o700);
        }
        // 没有留下临时文件
        assert_eq!(std::fs::read_dir(dir.join("sub")).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
