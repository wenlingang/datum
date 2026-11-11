# 维护者手册:测试、打包与发版

应用介绍、截图与下载在 [wenlingang.github.io/datum](https://wenlingang.github.io/datum/)(源码在 `site/`)。
这里只记开发与发版的细节。nsh 重写的设计说明见 `docs/superpowers/specs/2026-11-10-nsh-rewrite-design.md`。

## 测试

```bash
app/testing/run-tests.sh          # 全部界面测试 + Rust 测试,每块在各自的临时配置目录里跑
app/testing/run-tests.sh core     # 核心(CI 跑这个):Rust + 查询/表浏览 + 快捷键 + 连接表单等对话框
app/testing/run-tests.sh tab      # 只跑一块:workspace | tab | dialogs | chat | rust
cargo test --workspace            # datum-core 单元测试(数据层集成测试默认忽略)
```

### 数据层集成测试(需要数据库)

```bash
cargo test -p datum-core --lib -- --ignored --test-threads=1
```

可用环境变量覆盖测试连接:`DBSTUDIO_TEST_PG`(libpq 连接串)、`DBSTUDIO_TEST_MYSQL`(`host:port:user:password:db`)、
`DBSTUDIO_TEST_REDIS`(redis URL)。

PG / MySQL 上的只读执行(SQLite 用例不需要这些变量):

```bash
DATUM_TEST_PG_DB=<库名> DATUM_TEST_MYSQL_DB=<库名> cargo test -p datum-core --test read_only -- --ignored
```

TLS 用例按端口连本机库,未设置的变量对应用例直接跳过:

```bash
DATUM_TLS_PG_PLAIN=5432 DATUM_TLS_PG_SSL=5444 DATUM_TLS_MYSQL=3306 DATUM_TLS_REDIS=6379 \
  cargo test -p datum-core --test tls -- --ignored --test-threads=1
```

`DATUM_TLS_PG_SSL` 指向开启 `ssl=on`(自签证书即可)的 PG,`DATUM_TLS_PG_PLAIN` / `DATUM_TLS_REDIS` 指向未开加密的实例。

### 截图

介绍页的截图由 `app/testing/demo/` 生成(演示数据 `make-demo.sh`、截图 `screenshots.test.tsx`、处理 `publish.sh`),
在 macOS 上用 `nsh test --target macos` 实拍。

## CI

- `.github/workflows/ci.yml`:PR 跑核心测试;推到 `main` 时只在 nsh 版本变化(`app/native/Cargo.toml`)时自动跑,
  其余时候在 Actions 页面手动触发。
- nsh CLI 从源码编译约 20 分钟,`.github/actions/setup-nsh` 按 系统 + 架构 + nsh rev 缓存编译好的二进制。
  标签触发的运行只能读到 `main` 上写的缓存,所以**升级 nsh 后先在 main 上跑一次 CI** 把四个平台的缓存建好,再发版。
- 仓库是公开的,GitHub 托管 runner 不计费。

## 发版

1. 修改 `app/nsh.json` 的 `version`(版本号的唯一来源,`script/app-version.sh` 可打印)和 `build`,提交。
2. 可选试跑:Actions › Release › Run workflow,只打包不发布,可下载产物试装。
3. 推送附注标签 `v<版本>`,标签说明就是应用内显示的更新说明:

```bash
git tag -a v0.1.2 -m "本次更新内容"
git push origin v0.1.2
```

`.github/workflows/release.yml`:校验标签与版本一致 + 核心测试,与 macOS(aarch64、x86_64)/ Windows / Linux 上的
`nsh package` **并行** → macOS 用发版证书重签并做 dmg(`script/sign-macos.sh`)→ 两者都通过后 `nsh update-manifest`
签名 `latest.json` → 建 Release。版本号带 `-`(如 `v0.2.0-rc1`)时发为预发布,已安装的应用不会更新到它。

Release 附件:

| 文件 | 用途 |
| --- | --- |
| `Datum-<版本>-macos-aarch64.tar.gz` / `-x86_64.tar.gz` | macOS 更新包(应用内更新下载它) |
| `Datum-<版本>-macos-aarch64.dmg` / `-x86_64.dmg` | macOS 首次安装 |
| `Datum.dmg` | Apple 芯片 dmg 的固定名副本,介绍页的下载链接不会过期 |
| `Datum-<版本>-windows-x86_64.zip`、`Datum-setup.exe` | Windows 更新包与安装器 |
| `Datum-<版本>-linux-x86_64.tar.gz` | Linux |
| `latest.json`、`latest.json.sig` | 更新清单及其签名 |
| `SHA256SUMS` | 校验和 |

仓库 secrets(Settings → Secrets and variables → Actions):

- `RELEASE_CERT_P12` / `RELEASE_CERT_PASSWORD`:macOS 自签名发版证书 "Datum Release"
- `UPDATE_SIGNING_KEY`:更新清单签名私钥(`nsh keygen` 生成的 `update.secret` 的内容)

### 发版证书(macOS)

macOS 包用自签名证书 "Datum Release" 重签。**不要更换它**:钥匙串里对 staging / prod 密码的「始终允许」授权跟随签名,
换了证书,每位用户都得重新授权一遍。`script/release-cert.sha1` 钉住它的 SHA-1,`script/sign-macos.sh` 拒绝用其他证书签名。

一次性配置:

```bash
script/release-cert.sh        # 证书写到 ~/datum-release-cert/,指纹写到 script/release-cert.sha1
# 备份 datum-release.p12 和 password.txt,提交 script/release-cert.sha1,然后:
gh secret set RELEASE_CERT_P12 < ~/datum-release-cert/datum-release.p12.base64
gh secret set RELEASE_CERT_PASSWORD < ~/datum-release-cert/password.txt
```

### 更新签名密钥

私钥放在仓库外(`~/datum-update-key/update.secret`),务必备份;公钥是 `app/nsh.json` 里的 `updater.publicKey`,
已安装的应用拒绝没有用它签名的清单。

```bash
gh secret set UPDATE_SIGNING_KEY < ~/datum-update-key/update.secret
```

### 本地打包

nsh 只为当前所在的平台打包,在 `app/` 下执行:

```bash
cd app
nsh package --target macos       # 或 windows / linux;输出在 app/.nsh/out/
../script/sign-macos.sh .nsh/out aarch64   # 可选,macOS:用 "Datum Release" 重签并做 dmg
# 和已安装的 Datum 并存试用:--name datum-test --id com.dbstudio.app.test -o .nsh/out-test
```

`nsh package` 给 macOS 应用做的是 ad hoc 签名,本机试用没问题。

### 应用内更新

更新走 `nsh/updater`(见 `app/store/update.ts`):`nsh.json` 的 `updater` 指向本 GitHub 仓库并带公钥。打包后的应用启动 10 秒后
首次检查,之后每 4 小时检查一次最新 Release 里的 `latest.json`,校验签名后下载本平台的更新包,核对大小与 SHA-256,替换自身并重启。
开发运行(`nsh run`)不会更新。它与 v0.0.1 的旧更新器不兼容,旧版用户需要手动下载一次新版。
