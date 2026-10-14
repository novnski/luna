//! Files' Open rendered preview on a generated local HTML report (#12), in
//! native WebKit: the report renders with its own assets and subpages, its
//! links cannot leave the report folder, reopening reloads the same tab from
//! disk, and ordinary tabs still refuse `file:` addresses.
use super::*;
use gpui::{Entity, WindowHandle};

const REPORT: &str = "<!doctype html><meta charset=utf-8><title>{{title}}</title><style>body{margin:0;padding:28px;background:#f6f7f9;color:#1d2433;font:14px/1.5 system-ui}h1{font-size:21px;margin:0 0 14px}.row{display:flex;gap:10px;padding:10px 14px;margin:6px 0;border:1px solid #d9dde5;border-radius:8px;background:#fff}.pass{color:#18794e;font-weight:600}a{color:#2453b8}</style><h1>3 passed · 0 failed</h1><div class=row><span class=pass>✓</span>renders the landing page</div><div class=row><span class=pass>✓</span>opens the workspace cards</div><div class=row><span class=pass>✓</span>keeps history between pages</div><p><img id=chart src='data/chart.svg' alt='Duration chart'></p><p><a id=detail href='data/detail.html'>Open test details</a> · <a id=outside href='../outside.html'>Outside the report</a></p>";

async fn wait_until(
    cx: &mut AsyncApp,
    failure: &str,
    seconds: u64,
    mut done: impl FnMut(&mut AsyncApp) -> bool,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
    while !done(cx) {
        anyhow::ensure!(std::time::Instant::now() < deadline, "{failure}");
        pause(cx, 50).await;
    }
    Ok(())
}

fn title(browser: &Entity<browser::BrowserSurface>, cx: &mut AsyncApp) -> String {
    browser.read_with(cx, |b, _| b.page.title.clone())
}

pub(super) async fn exercise(
    window: WindowHandle<shell::Shell>,
    state: Entity<state::AppState>,
    output: &std::path::Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().canonicalize()?;
    std::fs::create_dir_all(root.join("playwright-report/data"))?;
    let index = root.join("playwright-report/index.html");
    std::fs::write(&index, REPORT.replace("{{title}}", "Local report"))?;
    std::fs::write(
        root.join("playwright-report/data/detail.html"),
        "<!doctype html><title>Report detail</title><a href='../index.html'>Back</a>",
    )?;
    std::fs::write(
        root.join("playwright-report/data/chart.svg"),
        "<svg xmlns='http://www.w3.org/2000/svg' width='96' height='24'><rect width='96' height='24' rx='4' fill='#18794e'/></svg>",
    )?;
    std::fs::write(
        root.join("outside.html"),
        "<!doctype html><title>Outside the report</title>",
    )?;
    let document = url::Url::from_file_path(&index)
        .map_err(|_| anyhow::anyhow!("report path is not a file URL"))?
        .to_string();
    let outside = url::Url::from_file_path(root.join("outside.html"))
        .map_err(|_| anyhow::anyhow!("outside path is not a file URL"))?
        .to_string();
    state.update(cx, |s, cx| {
        for chat in &mut s.chats {
            if chat.id == "browser-fixture" {
                chat.cwd = Some(root.to_string_lossy().into_owned());
            }
        }
        cx.notify();
    });
    pause(cx, 200).await;

    window.update(cx, |shell, w, cx| {
        shell.fixture_open_rendered_preview("playwright-report/index.html", w, cx)
    })?;
    let mut opened = None;
    wait_until(
        cx,
        "Open rendered preview did not open a local tab",
        5,
        |cx| {
            opened = window
                .update(cx, |shell, _, cx| shell.fixture_active_browser(cx))
                .ok()
                .flatten()
                .filter(|(_, b)| b.read_with(cx, |b, _| b.local_document().is_some()));
            opened.is_some()
        },
    )
    .await?;
    let (report_id, report) = opened.unwrap();
    anyhow::ensure!(
        report.read_with(cx, |b, _| b.page.url.as_deref() == Some(document.as_str())),
        "preview opened {:?}, not the report",
        report.read_with(cx, |b, _| b.page.url.clone())
    );
    wait_until(cx, "local report did not render", 25, |cx| {
        report.read_with(cx, |b, _| {
            b.page.title == "Local report" && !b.page.loading && b.fixture_native_visible()
        })
    })
    .await?;
    // A relative asset from the report folder loads beside the page.
    report.read_with(cx, |b, _| {
        b.fixture_eval("document.title = document.getElementById('chart').naturalWidth === 96 ? 'Chart loaded' : 'Chart missing'")
    });
    wait_until(cx, "report asset check did not run", 5, |cx| {
        title(&report, cx).starts_with("Chart")
    })
    .await?;
    anyhow::ensure!(
        title(&report, cx) == "Chart loaded",
        "report folder asset did not load"
    );
    report.read_with(cx, |b, _| b.fixture_eval("document.title = 'Local report'"));
    pause(cx, 400).await;
    capture(output, "browser-local-report")?;

    // The report's own subpages and history work like a website's.
    report.read_with(cx, |b, _| {
        b.fixture_eval("document.getElementById('detail').click()")
    });
    wait_until(cx, "report subpage did not open", 10, |cx| {
        report.read_with(cx, |b, _| {
            b.page.title == "Report detail" && b.page.can_back
        })
    })
    .await?;
    report.update(cx, |b, _| b.fixture_history(false));
    wait_until(cx, "history did not return to the report", 10, |cx| {
        title(&report, cx) == "Local report"
    })
    .await?;

    // A link out of the report folder is cancelled by the native policy, and
    // the address bar refuses the same file.
    report.read_with(cx, |b, _| {
        b.fixture_eval("document.getElementById('outside').click()")
    });
    pause(cx, 1500).await;
    anyhow::ensure!(
        report.read_with(cx, |b, _| {
            b.page.title == "Local report" && b.page.url.as_deref() == Some(document.as_str())
        }),
        "report navigated outside its folder: {:?}",
        report.read_with(cx, |b, _| b.page.clone())
    );
    window.update(cx, |_, w, cx| {
        report.update(cx, |b, cx| b.navigate(&outside, w, cx))
    })?;
    anyhow::ensure!(
        report.read_with(cx, |b, _| b.page.url.as_deref() == Some(document.as_str())),
        "address bar opened a file outside the report folder"
    );

    // A regenerated report: Open rendered preview again reuses this tab and
    // reloads the file from disk.
    std::fs::write(&index, REPORT.replace("{{title}}", "Updated report"))?;
    window.update(cx, |shell, w, cx| {
        shell.fixture_open_rendered_preview("playwright-report/index.html", w, cx)
    })?;
    wait_until(cx, "reopening did not reload the report tab", 15, |cx| {
        title(&report, cx) == "Updated report"
    })
    .await?;
    anyhow::ensure!(
        window
            .update(cx, |shell, _, cx| shell.fixture_active_browser(cx))?
            .is_some_and(|(id, _)| id == report_id),
        "reopening the report opened a second tab"
    );

    // Ordinary tabs keep refusing local files.
    let (web_id, web) =
        window.update(cx, |shell, w, cx| shell.fixture_open_browser(None, w, cx))?;
    window.update(cx, |_, w, cx| {
        web.update(cx, |b, cx| b.navigate(&document, w, cx))
    })?;
    anyhow::ensure!(
        web.read_with(cx, |b, _| b.page.url.is_none()),
        "an ordinary tab opened a file address"
    );
    window.update(cx, |shell, w, cx| {
        shell.fixture_close_browser(web_id, w, cx)
    })?;
    window.update(cx, |shell, w, cx| {
        shell.fixture_close_browser(report_id, w, cx)
    })?;
    pause(cx, 200).await;
    anyhow::ensure!(
        !report.read_with(cx, |b, _| b.fixture_native_visible()),
        "closed report tab retained its native view"
    );
    Ok(())
}
