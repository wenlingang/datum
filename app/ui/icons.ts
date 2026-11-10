// 由 assets/icons/*.svg 生成:每个图标一个静态 import(打包器据此带上资源)。
// 新增图标:把 svg 放进 assets/icons/ 后在这里补一行。

import activity from "../assets/icons/activity.svg";
import arrowUp from "../assets/icons/arrow-up.svg";
import braces from "../assets/icons/braces.svg";
import check from "../assets/icons/check.svg";
import chevronDown from "../assets/icons/chevron-down.svg";
import chevronLeft from "../assets/icons/chevron-left.svg";
import chevronRight from "../assets/icons/chevron-right.svg";
import chevronUp from "../assets/icons/chevron-up.svg";
import chevronsLeft from "../assets/icons/chevrons-left.svg";
import chevronsRight from "../assets/icons/chevrons-right.svg";
import chevronsUpDown from "../assets/icons/chevrons-up-down.svg";
import circleAlert from "../assets/icons/circle-alert.svg";
import circleCheck from "../assets/icons/circle-check.svg";
import circleX from "../assets/icons/circle-x.svg";
import codeXml from "../assets/icons/code-xml.svg";
import copy from "../assets/icons/copy.svg";
import cpu from "../assets/icons/cpu.svg";
import crosshair from "../assets/icons/crosshair.svg";
import database from "../assets/icons/database.svg";
import dbMongo from "../assets/icons/db-mongo.svg";
import dbMysql from "../assets/icons/db-mysql.svg";
import dbPg from "../assets/icons/db-pg.svg";
import dbRedis from "../assets/icons/db-redis.svg";
import dbSqlite from "../assets/icons/db-sqlite.svg";
import download from "../assets/icons/download.svg";
import ellipsis from "../assets/icons/ellipsis.svg";
import eraser from "../assets/icons/eraser.svg";
import eyeOff from "../assets/icons/eye-off.svg";
import eye from "../assets/icons/eye.svg";
import fileText from "../assets/icons/file-text.svg";
import fileUp from "../assets/icons/file-up.svg";
import folderOpen from "../assets/icons/folder-open.svg";
import funnel from "../assets/icons/funnel.svg";
import imageDown from "../assets/icons/image-down.svg";
import importIcon from "../assets/icons/import.svg";
import inbox from "../assets/icons/inbox.svg";
import info from "../assets/icons/info.svg";
import keyRound from "../assets/icons/key-round.svg";
import link2 from "../assets/icons/link-2.svg";
import listTree from "../assets/icons/list-tree.svg";
import loaderCircle from "../assets/icons/loader-circle.svg";
import lockOpen from "../assets/icons/lock-open.svg";
import lock from "../assets/icons/lock.svg";
import maximize from "../assets/icons/maximize.svg";
import monitor from "../assets/icons/monitor.svg";
import moon from "../assets/icons/moon.svg";
import network from "../assets/icons/network.svg";
import panelLeft from "../assets/icons/panel-left.svg";
import pencil from "../assets/icons/pencil.svg";
import play from "../assets/icons/play.svg";
import plus from "../assets/icons/plus.svg";
import quote from "../assets/icons/quote.svg";
import refreshCw from "../assets/icons/refresh-cw.svg";
import rotateCcwClock from "../assets/icons/rotate-ccw-clock.svg";
import rotateCcw from "../assets/icons/rotate-ccw.svg";
import scanSearch from "../assets/icons/scan-search.svg";
import search from "../assets/icons/search.svg";
import settings from "../assets/icons/settings.svg";
import sparkles from "../assets/icons/sparkles.svg";
import squarePlay from "../assets/icons/square-play.svg";
import sun from "../assets/icons/sun.svg";
import table2 from "../assets/icons/table-2.svg";
import table from "../assets/icons/table.svg";
import terminal from "../assets/icons/terminal.svg";
import textAlignStart from "../assets/icons/text-align-start.svg";
import trash from "../assets/icons/trash.svg";
import triangleAlert from "../assets/icons/triangle-alert.svg";
import type from "../assets/icons/type.svg";
import undo2 from "../assets/icons/undo-2.svg";
import wandSparkles from "../assets/icons/wand-sparkles.svg";
import x from "../assets/icons/x.svg";
import zoomIn from "../assets/icons/zoom-in.svg";
import zoomOut from "../assets/icons/zoom-out.svg";

export const ICONS = {
  "activity": activity,
  "arrow-up": arrowUp,
  "braces": braces,
  "check": check,
  "chevron-down": chevronDown,
  "chevron-left": chevronLeft,
  "chevron-right": chevronRight,
  "chevron-up": chevronUp,
  "chevrons-left": chevronsLeft,
  "chevrons-right": chevronsRight,
  "chevrons-up-down": chevronsUpDown,
  "circle-alert": circleAlert,
  "circle-check": circleCheck,
  "circle-x": circleX,
  "code-xml": codeXml,
  "copy": copy,
  "cpu": cpu,
  "crosshair": crosshair,
  "database": database,
  "db-mongo": dbMongo,
  "db-mysql": dbMysql,
  "db-pg": dbPg,
  "db-redis": dbRedis,
  "db-sqlite": dbSqlite,
  "download": download,
  "ellipsis": ellipsis,
  "eraser": eraser,
  "eye-off": eyeOff,
  "eye": eye,
  "file-text": fileText,
  "file-up": fileUp,
  "folder-open": folderOpen,
  "funnel": funnel,
  "image-down": imageDown,
  "import": importIcon,
  "inbox": inbox,
  "info": info,
  "key-round": keyRound,
  "link-2": link2,
  "list-tree": listTree,
  "loader-circle": loaderCircle,
  "lock-open": lockOpen,
  "lock": lock,
  "maximize": maximize,
  "monitor": monitor,
  "moon": moon,
  "network": network,
  "panel-left": panelLeft,
  "pencil": pencil,
  "play": play,
  "plus": plus,
  "quote": quote,
  "refresh-cw": refreshCw,
  "rotate-ccw-clock": rotateCcwClock,
  "rotate-ccw": rotateCcw,
  "scan-search": scanSearch,
  "search": search,
  "settings": settings,
  "sparkles": sparkles,
  "square-play": squarePlay,
  "sun": sun,
  "table-2": table2,
  "table": table,
  "terminal": terminal,
  "text-align-start": textAlignStart,
  "trash": trash,
  "triangle-alert": triangleAlert,
  "type": type,
  "undo-2": undo2,
  "wand-sparkles": wandSparkles,
  "x": x,
  "zoom-in": zoomIn,
  "zoom-out": zoomOut,
} as const;

export type IconName = keyof typeof ICONS;
