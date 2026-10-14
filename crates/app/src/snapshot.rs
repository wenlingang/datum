//! 开发工具:无窗口渲染截图。
//!
//! `DATUM_SNAPSHOT="conn:fx;table:users;wait:500;shot:/tmp/a.png"` 运行一段脚本后截图,
//! 用于在没有屏幕录制权限的环境里检查界面。仅在 `snapshot` feature 下编译。

use crate::actions;
use crate::store::Stores;
use crate::workspace::Workspace;
use gpui_kit::{
    AppContext as _, Bounds, Entity, HeadlessAppContext, WindowBounds, WindowOptions, px, size,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn pump(cx: &mut HeadlessAppContext, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    loop {
        cx.run_until_parked();
        if Instant::now() >= until {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// 无头模式没有显示器驱动的重绘:强制画一帧,让命中测试(hover/按下状态)跟上最新的鼠标事件。
fn draw_frame(cx: &mut HeadlessAppContext, handle: gpui_kit::AnyWindowHandle) {
    cx.update_window(handle, |_, window, _| window.refresh())
        .ok();
    pump(cx, 30);
    let _ = cx.capture_screenshot(handle);
}

pub fn run(script: String) {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(crate::assets::AppAssets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.allow_parking();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(crate::prefs::Prefs::load());
        crate::i18n::apply(cx);
        crate::theme::init(cx);
        Stores::init(cx);
        actions::bind_keys(cx);
    });
    let (w, h) = std::env::var("DATUM_SNAPSHOT_SIZE")
        .ok()
        .and_then(|s| {
            s.split_once('x')
                .map(|(a, b)| (a.parse().unwrap_or(1400.), b.parse().unwrap_or(900.)))
        })
        .unwrap_or((1400., 900.));
    let (handle, ws): (_, Entity<Workspace>) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(w), px(h)),
                    })),
                    focus: true,
                    show: false,
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Workspace::new(window, cx)),
            )
        })
        .expect("open window");
    pump(&mut cx, 200);

    for step in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (cmd, arg) = step.split_once(':').unwrap_or((step, ""));
        let arg = arg.to_string();
        if cmd == "wait" {
            pump(&mut cx, arg.parse().unwrap_or(300));
            continue;
        }
        if cmd == "shot" {
            cx.update_window(handle, |_, window, _| window.refresh())
                .ok();
            pump(&mut cx, 60);
            match cx.capture_screenshot(handle) {
                Ok(img) => {
                    img.save(&arg).expect("save png");
                    eprintln!("snapshot: {arg}");
                }
                Err(e) => eprintln!("snapshot failed: {e}"),
            }
            continue;
        }
        if cmd == "bench" {
            // 连续重绘 N 帧,输出平均每帧耗时(含 pan 变化,模拟拖动画布)
            let n: u32 = arg.parse().unwrap_or(30);
            let start = Instant::now();
            for i in 0..n {
                let ws = ws.clone();
                cx.update_window(handle, move |_, window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.snapshot_step("erd-pan", &i.to_string(), window, cx)
                    });
                    window.refresh();
                })
                .ok();
                cx.run_until_parked();
                let _ = cx.capture_screenshot(handle);
            }
            eprintln!(
                "bench: {:.1} ms/frame",
                start.elapsed().as_secs_f64() * 1000. / n as f64
            );
            continue;
        }
        if cmd == "click" {
            // click:x,y(逻辑像素)模拟一次左键单击
            if let Some((x, y)) = arg.split_once(',') {
                let pos = gpui_kit::point(px(x.parse().unwrap_or(0.)), px(y.parse().unwrap_or(0.)));
                // 先移过去再按:真实鼠标总有移动事件,hover 状态登记后点击才生效;
                // 按下与抬起之间也留一帧
                cx.update_window(handle, |_, window, cx| {
                    let mv = gpui_kit::MouseMoveEvent {
                        position: pos,
                        pressed_button: None,
                        modifiers: Default::default(),
                    };
                    window.dispatch_event(gpui_kit::PlatformInput::MouseMove(mv), cx);
                })
                .ok();
                draw_frame(&mut cx, handle);
                cx.update_window(handle, |_, window, cx| {
                    let down = gpui_kit::MouseDownEvent {
                        position: pos,
                        modifiers: Default::default(),
                        button: gpui_kit::MouseButton::Left,
                        click_count: 1,
                        first_mouse: false,
                    };
                    window.dispatch_event(gpui_kit::PlatformInput::MouseDown(down), cx);
                })
                .ok();
                draw_frame(&mut cx, handle);
                cx.update_window(handle, |_, window, cx| {
                    let up = gpui_kit::MouseUpEvent {
                        position: pos,
                        modifiers: Default::default(),
                        button: gpui_kit::MouseButton::Left,
                        click_count: 1,
                    };
                    window.dispatch_event(gpui_kit::PlatformInput::MouseUp(up), cx);
                })
                .ok();
            }
            pump(&mut cx, 200);
            continue;
        }
        if cmd == "key" {
            cx.update_window(handle, |_, window, cx| {
                if let Ok(k) = gpui_kit::Keystroke::parse(&arg) {
                    window.dispatch_keystroke(k, cx);
                }
            })
            .ok();
            pump(&mut cx, 150);
            continue;
        }
        let ws = ws.clone();
        cx.update_window(handle, move |_, window, cx| {
            ws.update(cx, |ws, cx| ws.snapshot_step(cmd, &arg, window, cx));
        })
        .ok();
        pump(&mut cx, 120);
    }
}
