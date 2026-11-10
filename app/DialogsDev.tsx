// 对话框子任务的临时验证入口(不属于正式应用;正式入口是 App.tsx)。
// DATUM_CONFIG_DIR=<临时目录> nsh run DialogsDev.tsx

import { mount } from "nsh";
import { bootDev, Dev } from "./dialogs/dev";

bootDev();
mount(Dev);
