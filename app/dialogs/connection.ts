// 连接表单的纯逻辑:草稿 ↔ Connection、按类型填默认值、连接串预览、保存前校验。
// 对应 GPUI 版 dialogs/connection_form.rs 里不涉及界面的部分。

import { t } from "nsh/i18n";
import { type Connection, type DbKind, type Env, KIND_SCHEME, type SshAuth, type SslMode } from "../lib/types";

/** 表单草稿:输入框里的原文(端口也是文本)。 */
export interface ConnDraft {
  kind: DbKind;
  env: Env;
  ssl: SslMode;
  name: string;
  host: string;
  port: string;
  user: string;
  password: string;
  database: string;
  filePath: string;
  /** SSH 隧道(SQLite 不适用) */
  ssh: boolean;
  sshHost: string;
  sshPort: string;
  sshUser: string;
  sshAuth: SshAuth;
  sshKeyPath: string;
  /** SSH 密码或私钥密码短语;编辑时留空 = 沿用已保存的 */
  sshSecret: string;
  /** 先经过的跳板(ProxyJump 写法) */
  sshProxyJump: string;
}

const NO_SSH = {
  ssh: false,
  sshHost: "",
  sshPort: "22",
  sshUser: "",
  sshAuth: "password" as SshAuth,
  sshKeyPath: "",
  sshSecret: "",
  sshProxyJump: "",
};

/** 新建时的初始值(PostgreSQL 本地库)。 */
export function emptyDraft(): ConnDraft {
  return {
    kind: "pg",
    env: "local",
    ssl: "prefer",
    name: "",
    host: "127.0.0.1",
    port: "5432",
    user: "postgres",
    password: "",
    database: "postgres",
    filePath: "",
    ...NO_SSH,
  };
}

/** 已有连接 → 草稿。`database` 给定时预填库名(切库后编辑)。 */
export function draftFrom(conn: Connection | undefined, database?: string): ConnDraft {
  if (!conn) {
    const d = emptyDraft();
    return database ? { ...d, database } : d;
  }
  const db = database ?? conn.database;
  return {
    kind: conn.kind,
    env: conn.env,
    ssl: conn.sslMode,
    name: conn.name,
    host: conn.host,
    port: conn.port === 0 ? "" : String(conn.port),
    user: conn.user,
    password: "",
    database: db,
    filePath: conn.filePath ?? "",
    ...(conn.ssh
      ? {
          ssh: true,
          sshHost: conn.ssh.host,
          sshPort: conn.ssh.port === 0 ? "" : String(conn.ssh.port),
          sshUser: conn.ssh.user,
          sshAuth: conn.ssh.auth,
          sshKeyPath: conn.ssh.keyPath,
          sshSecret: "",
          sshProxyJump: conn.ssh.proxyJump ?? "",
        }
      : NO_SSH),
  };
}

/** 切换类型:按类型填入惯用默认值(SQLite 只能是本地)。 */
export function withKind(d: ConnDraft, kind: DbKind): ConnDraft {
  if (kind === "sqlite") return { ...d, kind, env: "local" };
  const [port, user, database] =
    kind === "redis"
      ? ["6379", d.user, "0"]
      : kind === "mysql"
        ? ["3306", "root", ""]
        : ["5432", "postgres", "postgres"];
  return { ...d, kind, port, user, database };
}

const parsePort = (v: string): number => {
  const port = Number.parseInt(v.trim(), 10);
  return Number.isInteger(port) && port >= 0 && port <= 65535 ? port : 0;
};

/** 草稿 → 连接。SSH 的 `secret` 留 null,由 [`connectionAndPassword`] 按是否沿用填写。 */
export function buildConnection(d: ConnDraft, id: string): Connection {
  const sqlite = d.kind === "sqlite";
  return {
    id,
    name: d.name.trim(),
    kind: d.kind,
    env: d.env,
    host: sqlite ? "" : d.host.trim(),
    port: sqlite ? 0 : parsePort(d.port),
    user: sqlite ? "" : d.user.trim(),
    database: sqlite ? "" : d.database.trim(),
    plaintextPassword: null,
    filePath: sqlite ? d.filePath.trim() : null,
    sslMode: d.ssl,
    uri: null,
    authSource: null,
    ssh:
      sqlite || !d.ssh
        ? null
        : {
            host: d.sshHost.trim(),
            port: d.sshPort.trim() === "" ? 22 : parsePort(d.sshPort),
            user: d.sshUser.trim(),
            auth: d.sshAuth,
            keyPath: d.sshAuth === "key" ? d.sshKeyPath.trim() : "",
            secret: null,
            proxyJump: d.sshProxyJump.trim(),
          },
  };
}

/** 表单当前对应的 (连接, 密码)。密码 null 表示编辑时沿用已存的。 */
export function connectionAndPassword(
  d: ConnDraft,
  id: string,
  editing: boolean,
): { conn: Connection; password: string | null } {
  const conn = buildConnection(d, id);
  // SSH 密钥同理:编辑时留空 = 沿用已保存的;agent 认证不需要
  if (conn.ssh) {
    conn.ssh.secret = conn.ssh.auth === "agent" ? "" : editing && d.sshSecret === "" ? null : d.sshSecret;
  }
  // 编辑时密码留空 = 不修改;新建时留空 = 无密码
  return { conn, password: editing && d.password === "" ? null : d.password };
}

/** 保存前校验:返回给用户看的错误,通过返回 null。 */
export function validate(conn: Connection): string | null {
  if (!conn.name) return t("conn_form.name_required");
  if (conn.kind === "sqlite" && !conn.filePath) return t("conn_form.file_required");
  if (conn.ssh && !conn.ssh.host) return t("conn_form.ssh_host_required");
  return null;
}

const or = (v: string, d: string) => (v.trim() === "" ? d : v);

const SSL_QUERY: Record<SslMode, string> = {
  disable: "?sslmode=disable",
  prefer: "?sslmode=prefer",
  require: "?sslmode=require",
  verifyFull: "?sslmode=verify-full",
};

/** 连接串预览(密码以圆点掩码)。 */
export function previewString(d: ConnDraft): string {
  if (d.kind === "sqlite") return `sqlite://${d.filePath === "" ? t("conn_form.file_preview") : d.filePath}`;
  const host = or(d.host, "127.0.0.1");
  const user = or(d.user, "postgres");
  const db = or(d.database, "postgres");
  const auth = d.password === "" ? `${user}@` : `${user}:••••@`;
  const port = d.port.trim() === "" ? "" : `:${d.port.trim()}`;
  const redis = d.kind === "redis";
  const scheme = redis && (d.ssl === "require" || d.ssl === "verifyFull") ? "rediss" : KIND_SCHEME[d.kind];
  const query = redis ? "" : SSL_QUERY[d.ssl];
  const jump = d.ssh && d.sshProxyJump.trim() ? ` ⇢ ${d.sshProxyJump.trim()}` : "";
  const sshUser = d.sshUser.trim() ? `${d.sshUser.trim()}@` : "";
  const via = d.ssh ? `${jump}  ⇢ ssh ${sshUser}${or(d.sshHost, "?")}:${or(d.sshPort, "22")}` : "";
  return `${scheme}://${auth}${host}${port}/${db}${query}${via}`;
}

/** SSL 档位的展示名。 */
export function sslLabel(m: SslMode): string {
  switch (m) {
    case "disable":
      return t("conn_form.ssl_disable");
    case "prefer":
      return t("conn_form.ssl_prefer");
    case "require":
      return t("conn_form.ssl_require");
    case "verifyFull":
      return t("conn_form.ssl_verify_full");
  }
}
