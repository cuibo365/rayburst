// Cloud Drive bridge injected into the Baidu Netdisk webview (pan.baidu.com).
//
// Responsibilities:
//   1. Mount a "下载到 Rayburst" button in the file-list toolbar (same mount
//      point and UI classes as LinkSwift uses).
//   2. Read the user's selected files from the page (three fallback sources).
//   3. Invoke the Rust command `cloud_drive_baidu_download` so the backend can
//      exchange cookies -> access_token -> dlink -> aria2.
//   4. Provide a floating "返回 Rayburst" button that closes the embedded view.
//
// The Tauri IPC bridge is injected into this webview by Tauri itself; the
// backend grants the pan.baidu.com origin access through capabilities/remote.json.
(function () {
  'use strict';

  if (window.location.hostname !== 'pan.baidu.com' || !/^\/disk\//.test(window.location.pathname)) {
    return;
  }

  var api = window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke;
  if (!api) {
    console.warn('[rayburst] Tauri bridge unavailable in this webview; download button disabled.');
    return;
  }

  var TOOLBAR_SELECTOR = '.wp-s-agile-tool-bar__header';
  var BUTTON_CLASS = 'pl-button-rayburst';
  var FLOAT_CLASS = 'rayburst-float-back';
  var POLL_INTERVAL = 600;

  function waitFor(selector, callback, timeoutMs) {
    timeoutMs = timeoutMs || 20000;
    var start = Date.now();
    var timer = setInterval(function () {
      var el = document.querySelector(selector);
      if (el) {
        clearInterval(timer);
        callback(el);
        return;
      }
      if (Date.now() - start > timeoutMs) {
        clearInterval(timer);
      }
    }, POLL_INTERVAL);
  }

  // Read the files the user has checked on the page. Mirrors LinkSwift's
  // getSelectedList(): tries the Vue instances first, then the context store.
  function getSelectedList() {
    try {
      var list = [];
      var fileList = document.querySelector('.file-list');
      if (
        fileList && fileList.__vue__ && fileList.__vue__.allFileList && fileList.__vue__.allFileList[0]
      ) {
        list = fileList.__vue__.allFileList.filter(function (item) { return !!item.selected; });
      }
      if (!list.length) {
        var wpCore = document.querySelector('.wp-s-core-pan');
        if (
          wpCore && wpCore.__vue__ && wpCore.__vue__.selectedList && wpCore.__vue__.selectedList[0]
        ) {
          list = wpCore.__vue__.selectedList;
        }
      }
      if (!list.length && window.require) {
        var ctx = window.require('system-core:context/context.js');
        if (ctx && ctx.instanceForSystem && ctx.instanceForSystem.list && ctx.instanceForSystem.list.getSelected) {
          var sel = ctx.instanceForSystem.list.getSelected();
          if (sel && sel[0]) list = sel;
        }
      }
      return list || [];
    } catch (e) {
      console.error('[rayburst] getSelectedList failed', e);
      return [];
    }
  }

  function toPayload(list) {
    var out = [];
    list.forEach(function (f) {
      out.push({
        fs_id: f.fs_id,
        name: f.server_filename || f.name || '',
        size: f.size || 0,
        path: f.path || '',
        isdir: !!f.isdir
      });
    });
    return out;
  }

  // The folder currently open in the browser (address bar `path` param, with
  // Vue-store fallbacks). Used by the backend to mirror the Baidu folder
  // structure under the default download directory.
  function getCurrentDir() {
    try {
      var m = window.location.search.match(/[?&]path=([^&]*)/);
      if (m && m[1]) return decodeURIComponent(m[1]);
    } catch (e) { /* fall through to Vue store */ }
    try {
      var fileList = document.querySelector('.file-list');
      if (fileList && fileList.__vue__) {
        var cur =
          fileList.__vue__.currentDir ||
          fileList.__vue__.currentPath ||
          (fileList.__vue__.$parent && fileList.__vue__.$parent.currentDir) ||
          '';
        if (cur) return cur;
      }
      var wpCore = document.querySelector('.wp-s-core-pan');
      if (wpCore && wpCore.__vue__) {
        var route = wpCore.__vue__.route;
        if (route && route.query && route.query.path) return route.query.path;
        if (wpCore.__vue__.currentDir) return wpCore.__vue__.currentDir;
      }
    } catch (e) { /* ignore */ }
    return '';
  }

  var toastTimer = null;
  function toast(message, isError) {
    var existing = document.querySelector('.rayburst-toast');
    if (existing) existing.remove();
    var el = document.createElement('div');
    el.className = 'rayburst-toast';
    el.textContent = message;
    el.style.cssText =
      'position:fixed;left:50%;top:64px;transform:translateX(-50%);z-index:2147483647;' +
      'padding:10px 18px;border-radius:6px;font-size:13px;color:#fff;' +
      'background:' + (isError ? 'rgba(220,60,60,.92)' : 'rgba(40,40,40,.92)') + ';' +
      'box-shadow:0 4px 12px rgba(0,0,0,.25);max-width:70%;word-break:break-all;';
    document.body.appendChild(el);
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { el.remove(); }, 4000);
  }

  var busy = false;
  function onDownloadClick() {
    if (busy) return;
    var list = getSelectedList();
    if (!list.length) {
      toast('请先在文件列表中勾选要下载的文件', true);
      return;
    }
    var files = toPayload(list);
    if (!files.length) {
      toast('勾选的项中没有可下载的文件', true);
      return;
    }
    busy = true;
    api('cloud_drive_baidu_download', { files: files, current_dir: getCurrentDir() })
      .then(function (result) {
        toast(result && result.message ? result.message : '已添加 ' + files.length + ' 个文件到下载队列', false);
      })
      .catch(function (err) {
        var msg = (err && (err.message || err)) || '下载失败';
        toast('下载失败：' + String(msg), true);
      })
      .finally(function () { busy = false; });
  }

  function mountToolbarButton() {
    if (document.querySelector('.' + BUTTON_CLASS)) return;
    waitFor(TOOLBAR_SELECTOR, function (toolbar) {
      if (document.querySelector('.' + BUTTON_CLASS)) return;
      var host = document.createElement('div');
      host.className = 'wp-s-agile-tool-bar__h-group ' + BUTTON_CLASS;
      host.innerHTML =
        '<div class="wp-s-agile-tool-bar__h-action is-need-left-sep is-main">' +
        '<button class="u-button nd-file-list-toolbar-action-item u-button--primary u-button--small is-round is-has-icon ' + BUTTON_CLASS + '__btn">' +
        '<i class="u-icon u-icon-download"></i><span>下载到 Rayburst</span></button></div>';
      host.querySelector('button').addEventListener('click', onDownloadClick);
      toolbar.prepend(host);
    });
  }

  function mountFloatingBack() {
    if (document.querySelector('.' + FLOAT_CLASS)) return;
    var btn = document.createElement('div');
    btn.className = FLOAT_CLASS;
    btn.textContent = '返回 Rayburst';
    btn.setAttribute('title', '退出网盘浏览，返回 Rayburst 主界面');
    btn.style.cssText =
      'position:fixed;right:18px;top:14px;z-index:2147483646;' +
      'padding:6px 14px;border-radius:16px;font-size:12px;color:#fff;cursor:pointer;' +
      'background:rgba(24,24,24,.85);border:1px solid rgba(255,255,255,.25);' +
      'box-shadow:0 2px 8px rgba(0,0,0,.3);user-select:none;';
    btn.addEventListener('mouseenter', function () { btn.style.background = 'rgba(60,60,60,.9)'; });
    btn.addEventListener('mouseleave', function () { btn.style.background = 'rgba(24,24,24,.85)'; });
    btn.addEventListener('click', function () {
      try { api('cloud_drive_baidu_close'); } catch (e) { /* ignore */ }
    });
    document.body.appendChild(btn);
  }

  // Mount on the initial page and re-mount after SPA route changes
  // (the toolbar is rebuilt when the user navigates between folders).
  var lastPath = window.location.pathname;
  setInterval(function () {
    if (window.location.pathname !== lastPath) {
      lastPath = window.location.pathname;
      mountToolbarButton();
    }
  }, 800);

  mountToolbarButton();
  mountFloatingBack();
})();
