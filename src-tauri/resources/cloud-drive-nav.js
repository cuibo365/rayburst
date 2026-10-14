// cloud-drive-nav.js
//
// Full-screen navigation mode for Baidu Netdisk: the main webview navigates
// away from the app UI to https://pan.baidu.com/disk/main. This script is
// injected into every page of the MAIN window (tauri.conf.json
// app.windows[0].initializationScript) and only acts on Baidu hosts,
// adding a floating "back to Rayburst" button so the user can return.
(function () {
  'use strict';
  if (typeof window.__rayburstNavBtn !== 'undefined') return;
  var host = String(window.location.host);
  if (host.indexOf('pan.baidu.com') === -1 && host.indexOf('disk.baidu.com') === -1) return;
  window.__rayburstNavBtn = true;

  var btn = document.createElement('div');
  btn.textContent = '\u2190 \u8FD4\u56DE Rayburst';
  btn.setAttribute(
    'style',
    'position:fixed;bottom:16px;right:16px;z-index:2147483647;background:#1677ff;color:#fff;' +
      'padding:9px 16px;border-radius:8px;font:13px/1.4 system-ui,-apple-system,sans-serif;' +
      'cursor:pointer;box-shadow:0 2px 10px rgba(0,0,0,.25);user-select:none;white-space:nowrap;',
  );
  btn.addEventListener('click', function () {
    window.location.href = 'tauri://localhost';
  });

  function attach() {
    if (document.body) {
      document.body.appendChild(btn);
    } else {
      document.documentElement.appendChild(btn);
    }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', attach, { once: true });
  } else {
    attach();
  }
})();
