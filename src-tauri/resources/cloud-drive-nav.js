// cloud-drive-nav.js
//
// Full-screen navigation mode for Baidu Netdisk: the main window's webview
// navigates away from the app UI to https://pan.baidu.com/disk/main. This
// script is injected into the MAIN window's pages (tauri.conf.json
// initializationScript) AND re-injected via Rust eval (spawn_nav_injector).
// It only acts on Baidu hosts, adding:
//   - top-right window controls: minimize / close (call Rust commands)
//   - bottom-right "back to Rayburst" button (navigates back to the app)
(function () {
  'use strict';
  if (typeof window.__rayburstNavBtn !== 'undefined') return;
  var host = String(window.location.host);
  if (host.indexOf('pan.baidu.com') === -1 && host.indexOf('disk.baidu.com') === -1) return;
  window.__rayburstNavBtn = true;

  var hasTauri =
    typeof window.__TAURI__ !== 'undefined' &&
    window.__TAURI__.core &&
    typeof window.__TAURI__.core.invoke === 'function';

  function invoke(cmd) {
    if (!hasTauri) return;
    try {
      window.__TAURI__.core.invoke(cmd);
    } catch (e) {
      // ignore: command may not be permitted yet
    }
  }

  // --- top-right window controls (minimize / close) ---
  var bar = document.createElement('div');
  bar.setAttribute(
    'style',
    'position:fixed;top:10px;right:10px;z-index:2147483647;display:flex;gap:6px;',
  );
  function wcBtn(symbol, title, cmd) {
    var b = document.createElement('div');
    b.textContent = symbol;
    b.title = title;
    b.setAttribute(
      'style',
      'width:36px;height:32px;display:flex;align-items:center;justify-content:center;' +
        'background:rgba(20,20,20,.55);color:#fff;border-radius:6px;' +
        'font:15px/1 system-ui,-apple-system,sans-serif;cursor:pointer;user-select:none;' +
        'box-shadow:0 1px 4px rgba(0,0,0,.3);',
    );
    b.addEventListener('click', function () {
      invoke(cmd);
    });
    return b;
  }
  bar.appendChild(wcBtn('\u2014', '\u6700\u5C0F\u5316', 'cloud_drive_nav_minimize'));
  bar.appendChild(wcBtn('\u2715', '\u5173\u95ED', 'cloud_drive_nav_close'));
  document.documentElement.appendChild(bar);

  // --- bottom-right back to app ---
  var back = document.createElement('div');
  back.textContent = '\u2190 \u8FD4\u56DE Rayburst';
  back.setAttribute(
    'style',
    'position:fixed;bottom:16px;right:16px;z-index:2147483647;background:#1677ff;color:#fff;' +
      'padding:9px 16px;border-radius:8px;font:13px/1.4 system-ui,-apple-system,sans-serif;' +
      'cursor:pointer;box-shadow:0 2px 10px rgba(0,0,0,.25);user-select:none;white-space:nowrap;',
  );
  back.addEventListener('click', function () {
    window.location.href = 'tauri://localhost';
  });
  document.documentElement.appendChild(back);
})();
