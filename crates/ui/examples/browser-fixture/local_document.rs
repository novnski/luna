//! A generated local HTML report (#12) in a folder-scoped Browser tab, in
//! native WebKit: the report renders with its own assets and subpages, its
//! links cannot leave the report folder, even through a symlink, opening it
//! again reloads it from disk, and ordinary tabs still refuse `file:`
//! addresses. Files' Preview control, its event and the shell's tab reuse
//! are owned by the headless `rendered_preview_regressions` test; this
//! scenario starts where they hand the document to the tab.
use super::*;
use gpui::{Entity, WindowHandle};
use luna_ui::browser::model::LocalDocument;

const REPORT: &str = "<!doctype html><meta charset=utf-8><title>{{title}}</title><style>body{margin:0;padding:28px;background:#f6f7f9;color:#1d2433;font:14px/1.5 system-ui}h1{font-size:21px;margin:0 0 14px}.row{display:flex;gap:10px;padding:10px 14px;margin:6px 0;border:1px solid #d9dde5;border-radius:8px;background:#fff}.pass{color:#18794e;font-weight:600}a{color:#2453b8}</style><h1>3 passed · 0 failed</h1><div class=row><span class=pass>✓</span>renders the landing page</div><div class=row><span class=pass>✓</span>opens the workspace cards</div><div class=row><span class=pass>✓</span>keeps history between pages</div><p><img id=chart src='data/chart.svg' alt='Duration chart'></p><p><a id=detail href='data/detail.html'>Open test details</a> · <a id=outside href='../outside.html'>Outside the report</a> · <a id=linked href='data/linked.html'>Linked outside</a></p>";

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
    std::os::unix::fs::symlink(
        root.join("outside.html"),
        root.join("playwright-report/data/linked.html"),
    )?;
    let preview = LocalDocument::in_workspace(&root, "playwright-report/index.html")
        .map_err(anyhow::Error::msg)?;
    let document = preview.url().to_owned();
    let outside = url::Url::from_file_path(root.join("outside.html"))
        .map_err(|_| anyhow::anyhow!("outside path is not a file URL"))?
        .to_string();

    let (report_id, report) =
        window.update(cx, |shell, w, cx| shell.fixture_open_browser(None, w, cx))?;
    window.update(cx, |_, w, cx| {
        report.update(cx, |b, cx| b.open_local(preview.clone(), w, cx))
    })?;
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

    // A typed address inside the folder loads; a `localhost` authority is
    // normalized away before it reaches WebKit.
    let detail = root.join("playwright-report/data/detail.html");
    let typed = format!("file://localhost{}", detail.display());
    window.update(cx, |_, w, cx| {
        report.update(cx, |b, cx| b.navigate(&typed, w, cx))
    })?;
    wait_until(cx, "typed report address did not load", 10, |cx| {
        title(&report, cx) == "Report detail"
    })
    .await?;
    anyhow::ensure!(
        report.read_with(cx, |b, _| b.page.url.as_deref()
            == url::Url::from_file_path(&detail)
                .ok()
                .as_ref()
                .map(url::Url::as_str)),
        "typed report address kept its localhost authority"
    );
    report.update(cx, |b, _| b.fixture_history(false));
    wait_until(cx, "history did not return to the report", 10, |cx| {
        title(&report, cx) == "Local report"
    })
    .await?;

    // Links out of the report folder, directly or through a symlink inside
    // it, are cancelled by the native policy; the address bar refuses the
    // same file.
    for link in ["outside", "linked"] {
        report.read_with(cx, |b, _| {
            b.fixture_eval(&format!("document.getElementById('{link}').click()"))
        });
        pause(cx, 1500).await;
        anyhow::ensure!(
            report.read_with(cx, |b, _| {
                b.page.title == "Local report" && b.page.url.as_deref() == Some(document.as_str())
            }),
            "report link {link} navigated outside its folder: {:?}",
            report.read_with(cx, |b, _| b.page.clone())
        );
    }
    window.update(cx, |_, w, cx| {
        report.update(cx, |b, cx| b.navigate(&outside, w, cx))
    })?;
    anyhow::ensure!(
        report.read_with(cx, |b, _| b.page.url.as_deref() == Some(document.as_str())),
        "address bar opened a file outside the report folder"
    );

    // A regenerated report: opening the document again reloads it from disk.
    std::fs::write(&index, REPORT.replace("{{title}}", "Updated report"))?;
    window.update(cx, |_, w, cx| {
        report.update(cx, |b, cx| b.open_local(preview.clone(), w, cx))
    })?;
    wait_until(cx, "reopening did not reload the report tab", 15, |cx| {
        title(&report, cx) == "Updated report"
    })
    .await?;

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
