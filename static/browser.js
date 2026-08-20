'use strict';

import { apiRequest, rawUpload } from './browser-api.js';
import { formatSize, state } from './browser-state.js';

const elements = Object.fromEntries([
  'adminButton','logoutButton','uploadButton','newFolderButton','searchInput',
  'breadcrumb','selectAllButton','fileList',
  'emptyState','fileInput','toast','contextMenu','folderModal','folderName','folderError',
  'createFolderButton','pickerModal','pickerTitle','pickerPath','pickerList',
  'pickerConfirmButton','adminModal','adminUser','adminPass','adminError','adminLoginButton',
  'uploadModal','uploadSummary','uploadFileName','uploadPercent','uploadProgress',
  'uploadBytes','uploadSpeed','uploadStatus','uploadResults','uploadCloseButton',
  'backToTopButton'
].map(id => [id, document.getElementById(id)]));

const request = apiRequest;

function fileApi(path = state.path) {
  return path ? `/api/files?path=${encodeURIComponent('/' + path)}` : '/api/files';
}

function actionApi(name, path = state.path) {
  return path ? `/api/${name}?path=${encodeURIComponent('/' + path)}` : `/api/${name}`;
}

function showToast(message) {
  elements.toast.textContent = message;
  elements.toast.classList.add('show');
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => elements.toast.classList.remove('show'), 2600);
}

function showModal(element) { element.classList.add('active'); }
function hideModal(element) { element.classList.remove('active'); }

function createFileIcon(entry) {
  const knownKinds = new Set(['image', 'video', 'audio', 'archive', 'pdf', 'code', 'doc']);
  const kind = entry.is_dir ? 'folder' : (knownKinds.has(entry.icon) ? entry.icon : 'file');
  const wrapper = document.createElement('span');
  wrapper.className = `file-icon ${kind}`;
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${kind}`);
  svg.append(use);
  wrapper.append(svg);
  if (entry.locked) {
    const lock = document.createElement('span');
    lock.className = 'lock-dot';
    lock.textContent = '●';
    lock.title = '文件夹已加锁';
    wrapper.append(lock);
  }
  return wrapper;
}

function visibleEntries() {
  const query = state.query.trim().toLocaleLowerCase();
  if (!query) return state.entries;
  return state.entries.filter(entry => entry.name.toLocaleLowerCase().includes(query));
}

function sortedEntries() {
  const direction = state.ascending ? 1 : -1;
  return [...visibleEntries()].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    let left;
    let right;
    if (state.sort === 'time') {
      left = a.modified;
      right = b.modified;
    } else if (state.sort === 'size') {
      left = a.is_dir ? -1 : a.size;
      right = b.is_dir ? -1 : b.size;
    } else {
      left = a.name.toLocaleLowerCase();
      right = b.name.toLocaleLowerCase();
    }
    return left < right ? -direction : left > right ? direction : 0;
  });
}

function renderBreadcrumb() {
  elements.breadcrumb.replaceChildren();
  const root = document.createElement('button');
  root.className = 'crumb';
  root.type = 'button';
  root.textContent = '/';
  root.title = '根目录';
  root.addEventListener('click', () => navigate(''));
  elements.breadcrumb.append(root);
  let accumulated = '';
  const parts = state.path.split('/').filter(Boolean);
  for (const part of parts) {
    accumulated = accumulated ? `${accumulated}/${part}` : part;
    const destination = accumulated;
    const separator = document.createElement('span');
    separator.className = 'crumb-separator';
    separator.textContent = '›';
    separator.setAttribute('aria-hidden', 'true');
    const button = document.createElement('button');
    button.className = 'crumb';
    button.type = 'button';
    button.textContent = part;
    button.addEventListener('click', () => navigate(destination));
    elements.breadcrumb.append(separator, button);
  }
  elements.breadcrumb.lastElementChild?.setAttribute('aria-current', 'location');
}

function createCell(className, text) {
  const cell = document.createElement('div');
  cell.className = className;
  cell.textContent = text;
  return cell;
}

function createFileRow(entry) {
  const row = document.createElement('div');
  row.className = `file-row${state.selected.has(entry.path) ? ' selected' : ''}`;
  row.dataset.path = entry.path;

  const selector = document.createElement('button');
  selector.type = 'button';
  selector.className = 'select-box';
  selector.setAttribute('aria-label', `选择 ${entry.name}`);
  selector.addEventListener('click', event => {
    event.stopPropagation();
    toggleSelection(entry.path);
  });

  const name = document.createElement('div');
  name.className = 'file-name';
  const label = document.createElement('span');
  label.className = 'file-label';
  label.textContent = entry.name;
  name.append(createFileIcon(entry), label);

  row.append(
    selector,
    name,
    createCell('cell right modified', entry.modified || '-'),
    createCell('cell right', entry.is_dir ? '-' : formatSize(entry.size))
  );
  row.addEventListener('click', () => {
    toggleSelection(entry.path);
  });
  row.addEventListener('dblclick', () => openEntry(entry));
  row.addEventListener('contextmenu', event => {
    if (isMobileLayout()) {
      event.preventDefault();
      return;
    }
    showContextMenu(event, entry);
  });
  return row;
}

function renderFiles() {
  const entries = sortedEntries();
  elements.fileList.replaceChildren(...entries.map(createFileRow));
  elements.emptyState.textContent = state.query ? '没有匹配的文件' : '此文件夹为空';
  elements.emptyState.classList.toggle('hidden', entries.length !== 0);
  for (const indicator of document.querySelectorAll('[data-indicator]')) {
    indicator.textContent = indicator.dataset.indicator === state.sort
      ? (state.ascending ? '▲' : '▼')
      : '';
  }
  updateSelectionControls();
}

function applyCapabilities() {
  for (const control of document.querySelectorAll('.write-control')) {
    const uploadBusy = state.uploading && control === elements.uploadButton;
    control.disabled = uploadBusy;
    control.classList.toggle('permission-required', !state.canWrite);
    control.setAttribute('aria-disabled', String(!state.canWrite || uploadBusy));
    control.title = state.canWrite ? '' : '登录管理员账号后可修改文件';
  }
}

async function refresh() {
  try {
    const data = await request(fileApi());
    state.path = (data.current_path || '').replace(/^\/+|\/+$/g, '');
    state.entries = data.entries || [];
    state.canWrite = data.can_write !== false;
    state.maxUploadBytes = Number(data.max_upload_bytes || 0);
    state.maxArchiveBytes = Number(data.max_archive_bytes || 0);
    state.maxArchiveEntries = Number(data.max_archive_entries || 0);
    state.selected.clear();
    if (data.truncated) showToast('目录内容超过服务器显示上限，当前仅显示部分项目');
    renderBreadcrumb();
    renderFiles();
    applyCapabilities();
  } catch (error) {
    showToast(error.message);
  }
}

function navigate(path) {
  state.path = String(path || '').replace(/^\/+|\/+$/g, '');
  state.query = '';
  elements.searchInput.value = '';
  refresh();
}

function toggleSelection(path) {
  if (state.selected.has(path)) state.selected.delete(path);
  else state.selected.add(path);
  const row = [...elements.fileList.children].find(item => item.dataset.path === path);
  row?.classList.toggle('selected', state.selected.has(path));
  updateSelectionControls();
}

function isMobileLayout() {
  return window.matchMedia('(max-width: 760px)').matches;
}

function syncMobileSelectionMenu() {
  const active = isMobileLayout() && state.selected.size > 0;
  document.body.classList.toggle('has-mobile-selection', active);
  if (!active) {
    hideContextMenu();
    return;
  }
  const selectedPath = state.selected.values().next().value;
  const entry = state.entries.find(item => item.path === selectedPath);
  if (entry) showContextMenu({ preventDefault() {}, clientX: 0, clientY: innerHeight }, entry);
}

function updateSelectionControls() {
  const entries = visibleEntries();
  const allSelected = entries.length > 0 && entries.every(entry => state.selected.has(entry.path));
  elements.selectAllButton.parentElement?.classList.toggle('selected', allSelected);
  syncMobileSelectionMenu();
}

function toggleSelectAll() {
  const entries = visibleEntries();
  const allSelected = entries.length > 0 && entries.every(entry => state.selected.has(entry.path));
  entries.forEach(entry => state.selected.delete(entry.path));
  if (!allSelected) entries.forEach(entry => state.selected.add(entry.path));
  renderFiles();
}

async function openEntry(entry) {
  if (!entry.is_dir) return preview(entry.path);
  if (entry.locked) return unlockFolder(entry.path);
  navigate(entry.path);
}

async function unlockFolder(path) {
  const password = await window._spPrompt('此文件夹已锁定，请输入密码：', '', 'password');
  if (!password) return;
  try {
    const result = await request('/api/folder/unlock', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path, password })
    });
    if (!result.success) throw new Error(result.message || '密码错误');
    navigate(path);
  } catch (error) { showToast(error.message); }
}

function preview(path) { window.open(`/preview.html?path=${encodeURIComponent('/' + path)}`, '_blank', 'noopener'); }
function download(path) { location.href = `/api/download?path=${encodeURIComponent('/' + path)}`; }

async function downloadArchive(paths) {
  try {
    const response = await fetch('/api/archive/prepare', {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ paths: paths.map(path => '/' + path) })
    });
    if (response.status === 401) {
      location.href = '/';
      return;
    }
    const result = await response.json();
    if (!response.ok) {
      if (response.status === 413) {
        throw new Error(`所选文件总大小超过 ${formatSize(state.maxArchiveBytes)}，请拆分选择`);
      }
      if (response.status === 400 && /entry (?:limit|safety limit)|entry limit/i.test(result?.error?.message || '')) {
        throw new Error(`打包最多包含 ${state.maxArchiveEntries || 1000} 个条目（文件与文件夹合计），请拆分选择`);
      }
      throw new Error(result?.error?.message || `打包准备失败 (${response.status})`);
    }
    showToast(`正在打包 ${result.file_count} 个文件、${result.entry_count} 个条目（${formatSize(result.total_bytes)}）`);
    location.href = `/api/archive?ticket=${encodeURIComponent(result.ticket)}`;
  } catch (error) { showToast(error.message); }
}

const uploadRequest = rawUpload;

function updateUploadProgress(processedBytes, currentLoaded, totalBytes, startedAt) {
  const transferred = Math.min(totalBytes, processedBytes + currentLoaded);
  const percent = totalBytes ? Math.min(100, Math.round(transferred / totalBytes * 100)) : 100;
  const elapsedSeconds = Math.max(.1, (performance.now() - startedAt) / 1000);
  elements.uploadPercent.textContent = `${percent}%`;
  elements.uploadProgress.value = percent;
  elements.uploadBytes.textContent = `${formatSize(transferred)} / ${formatSize(totalBytes)}`;
  elements.uploadSpeed.textContent = `${formatSize(transferred / elapsedSeconds)}/s`;
}

function createUploadResult(file) {
  const row = document.createElement('div');
  row.className = 'upload-result';
  const name = document.createElement('span');
  name.className = 'upload-result-name';
  name.textContent = file.name;
  const status = document.createElement('span');
  status.className = 'upload-result-state';
  status.textContent = '等待';
  row.append(name, status);
  elements.uploadResults.append(row);
  return { row, status };
}

function setUploadResult(result, stateName, message) {
  result.row.className = `upload-result ${stateName}`;
  result.status.textContent = message;
}

async function uploadFiles(files) {
  if (!state.canWrite || !files.length) return;
  const queue = Array.from(files);
  const uploaded = [];
  const failed = [];
  const accepted = queue.filter(file => !state.maxUploadBytes || file.size <= state.maxUploadBytes);
  const totalBytes = accepted.reduce((total, file) => total + file.size, 0);
  let processedBytes = 0;
  const startedAt = performance.now();
  state.uploading = true;
  elements.uploadButton.disabled = true;
  elements.uploadCloseButton.disabled = true;
  elements.uploadStatus.classList.remove('error');
  elements.uploadResults.replaceChildren();
  const resultRows = queue.map(createUploadResult);
  elements.uploadSummary.textContent = `共 ${queue.length} 个文件，逐个安全上传`;
  elements.uploadFileName.textContent = '正在准备…';
  updateUploadProgress(0, 0, totalBytes, startedAt);
  showModal(elements.uploadModal);
  try {
    for (let index = 0; index < queue.length; index += 1) {
      const file = queue[index];
      if (state.maxUploadBytes && file.size > state.maxUploadBytes) {
        failed.push({ name: file.name, message: `超过 ${formatSize(state.maxUploadBytes)}` });
        setUploadResult(resultRows[index], 'failed', '超过上限');
        continue;
      }
      elements.uploadFileName.textContent = file.name;
      elements.uploadStatus.textContent = `正在上传 ${index + 1}/${queue.length}`;
      setUploadResult(resultRows[index], '', '上传中');
      let fileTransferred = 0;
      try {
        const targetPath = [state.path, file.name].filter(Boolean).join('/');
        await uploadRequest(actionApi('upload', targetPath), file, (loaded, requestTotal) => {
          fileTransferred = requestTotal ? Math.min(file.size, loaded / requestTotal * file.size) : Math.min(file.size, loaded);
          updateUploadProgress(processedBytes, fileTransferred, totalBytes, startedAt);
        });
        fileTransferred = file.size;
        uploaded.push(file.name);
        setUploadResult(resultRows[index], 'success', '完成');
      } catch (error) {
        failed.push({ name: file.name, message: error.message });
        setUploadResult(resultRows[index], 'failed', error.message);
      }
      processedBytes += fileTransferred;
      updateUploadProgress(processedBytes, 0, totalBytes, startedAt);
    }
    await refresh();
    if (!failed.length) {
      elements.uploadStatus.textContent = `上传完成：成功 ${uploaded.length} 个文件`;
    } else {
      const first = failed[0];
      elements.uploadStatus.textContent = `成功 ${uploaded.length} 个，失败 ${failed.length} 个：${first.name}（${first.message}）`;
      elements.uploadStatus.classList.add('error');
    }
  }
  finally {
    state.uploading = false;
    applyCapabilities();
    elements.uploadCloseButton.disabled = false;
    elements.fileInput.value = '';
  }
}

function openFolderModal(target = state.path) {
  if (!state.canWrite) {
    showAdmin();
    return;
  }
  state.mkdirTarget = String(target || '').replace(/^\/+|\/+$/g, '');
  elements.folderName.value = '';
  elements.folderError.textContent = '';
  elements.folderError.classList.add('hidden');
  showModal(elements.folderModal);
  requestAnimationFrame(() => elements.folderName.focus());
}

async function createFolder() {
  if (state.creatingFolder) return;
  const name = elements.folderName.value.trim();
  if (!name) return;
  state.creatingFolder = true;
  elements.createFolderButton.disabled = true;
  try {
    await request(actionApi('mkdir', state.mkdirTarget), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name })
    });
    hideModal(elements.folderModal);
    showToast('文件夹已创建');
    await refresh();
  } catch (error) {
    elements.folderError.textContent = error.message;
    elements.folderError.classList.remove('hidden');
  } finally {
    state.creatingFolder = false;
    elements.createFolderButton.disabled = false;
  }
}

async function rename(path) {
  const oldName = path.split('/').pop();
  const newName = await window._spPrompt('请输入新名称：', oldName);
  if (!newName || newName === oldName) return;
  try {
    await request(actionApi('rename'), {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path: '/' + path, new_name: newName })
    });
    showToast('重命名成功');
    await refresh();
  } catch (error) { showToast(error.message); }
}

async function deletePaths(paths) {
  if (!paths.length || !state.canWrite) return;
  if (!await window._spConfirm(`永久删除 ${paths.length} 个项目？此操作无法撤销。`)) return;
  try {
    const result = await request('/api/batch/delete', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ paths })
    });
    showToast(result.failed ? `已删除 ${result.success} 项，失败 ${result.failed} 项` : `已删除 ${paths.length} 个项目`);
    await refresh();
  } catch (error) { showToast(error.message); }
}

function showPicker(title, callback) {
  state.pickerCallback = callback;
  state.pickerPath = '';
  elements.pickerTitle.textContent = title;
  showModal(elements.pickerModal);
  loadPicker('');
}

async function loadPicker(path) {
  state.pickerPath = path;
  elements.pickerPath.textContent = path ? `/${path}` : '/ 根目录';
  elements.pickerList.replaceChildren();
  if (path) {
    const up = document.createElement('button');
    up.type = 'button';
    up.className = 'picker-row';
    up.textContent = '↩ 返回上级';
    up.addEventListener('click', () => loadPicker(path.split('/').slice(0, -1).join('/')));
    elements.pickerList.append(up);
  }
  try {
    const data = await request(fileApi(path));
    for (const entry of (data.entries || []).filter(item => item.is_dir && !item.locked)) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'picker-row';
      button.textContent = `📁 ${entry.name}`;
      button.addEventListener('click', () => loadPicker(entry.path));
      elements.pickerList.append(button);
    }
  } catch (error) { showToast(error.message); }
}

async function transfer(operation, paths) {
  if (!paths.length || !state.canWrite) return;
  showPicker(`${operation === 'move' ? '移动' : '复制'} ${paths.length} 个项目到…`, async target => {
    try {
      const result = await request(`/api/batch/${operation}`, {
        method: operation === 'move' ? 'PUT' : 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ paths, target: '/' + target })
      });
      showToast(result.failed
        ? `${operation === 'move' ? '移动' : '复制'}成功 ${result.success} 项，失败 ${result.failed} 项`
        : (operation === 'move' ? '移动成功' : '复制成功'));
      await refresh();
    } catch (error) { showToast(error.message); }
  });
}

function addMenuItem(label, icon, action, danger = false) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = `menu-item${danger ? ' danger' : ''}`;
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${icon}`);
  svg.append(use);
  const text = document.createElement('span');
  text.textContent = label;
  button.append(svg, text);
  button.addEventListener('click', () => { hideContextMenu(); action(); });
  menuActions().append(button);
}

function menuActions() {
  let actions = elements.contextMenu.querySelector('.menu-actions');
  if (!actions) {
    actions = document.createElement('div');
    actions.className = 'menu-actions';
    elements.contextMenu.append(actions);
  }
  return actions;
}

function addMenuSeparator() {
  const separator = document.createElement('div');
  separator.className = 'menu-separator';
  menuActions().append(separator);
}

function addMenuCaption(count, mobileOnly = false) {
  const caption = document.createElement('div');
  caption.className = `menu-caption${mobileOnly ? ' mobile-only' : ''}`;
  const text = document.createElement('span');
  text.textContent = `已选择 ${count} 项`;
  const clear = document.createElement('button');
  clear.type = 'button';
  clear.className = 'menu-clear';
  clear.textContent = '清除';
  clear.addEventListener('click', event => {
    event.stopPropagation();
    state.selected.clear();
    renderFiles();
  });
  caption.append(text, clear);
  elements.contextMenu.append(caption);
}

function showContextMenu(event, entry = null) {
  event.preventDefault();
  elements.contextMenu.replaceChildren();
  if (!entry) {
    if (!state.canWrite) return;
    addMenuItem('上传文件', 'upload', () => elements.fileInput.click());
    addMenuItem('新建文件夹', 'folder-plus', () => openFolderModal());
  } else {
    if (!state.selected.has(entry.path)) {
      state.selected.clear();
      state.selected.add(entry.path);
      renderFiles();
    }
    const paths = [...state.selected];
    if (paths.length > 1) {
      addMenuCaption(paths.length);
      addMenuItem('打包下载', 'archive', () => downloadArchive(paths));
      if (state.canWrite) {
        addMenuSeparator();
        addMenuItem('移动', 'move', () => transfer('move', paths));
        addMenuItem('复制', 'copy', () => transfer('copy', paths));
        addMenuSeparator();
        addMenuItem(`删除 (${paths.length})`, 'trash', () => deletePaths(paths), true);
      }
    } else {
      addMenuCaption(1, true);
      if (entry.is_dir) {
        addMenuItem('打开', 'open', () => openEntry(entry));
        addMenuItem('打包下载', 'archive', () => downloadArchive(paths));
      }
      else {
        addMenuItem('下载', 'download', () => download(entry.path));
      }
      if (state.canWrite) {
        addMenuSeparator();
        addMenuItem('重命名', 'edit', () => rename(entry.path));
        addMenuItem('移动', 'move', () => transfer('move', paths));
        addMenuItem('复制', 'copy', () => transfer('copy', paths));
        addMenuSeparator();
        addMenuItem('删除', 'trash', () => deletePaths(paths), true);
      }
    }
  }
  elements.contextMenu.classList.add('active');
  if (!isMobileLayout()) {
    const margin = 8;
    const width = elements.contextMenu.offsetWidth;
    const height = elements.contextMenu.offsetHeight;
    const left = Math.max(margin, Math.min(event.clientX, window.innerWidth - width - margin));
    const top = Math.max(margin, Math.min(event.clientY, window.innerHeight - height - margin));
    elements.contextMenu.style.left = `${left}px`;
    elements.contextMenu.style.top = `${top}px`;
    elements.contextMenu.style.right = 'auto';
    elements.contextMenu.style.bottom = 'auto';
  }
}

function hideContextMenu() {
  elements.contextMenu.classList.remove('active');
  elements.contextMenu.style.removeProperty('left');
  elements.contextMenu.style.removeProperty('top');
  elements.contextMenu.style.removeProperty('right');
  elements.contextMenu.style.removeProperty('bottom');
}

async function showAdmin() {
  try {
    const identity = await request('/api/me');
    if (identity.is_admin) return location.href = '/admin';
  } catch (_) { return; }
  elements.adminUser.value = '';
  elements.adminPass.value = '';
  elements.adminError.classList.add('hidden');
  showModal(elements.adminModal);
  elements.adminPass.focus();
}

async function adminLogin() {
  const username = elements.adminUser.value.trim();
  const password = elements.adminPass.value;
  if (!username || !password) {
    elements.adminError.textContent = '请输入用户名和密码';
    elements.adminError.classList.remove('hidden');
    return;
  }
  try {
    const result = await request('/api/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password })
    });
    if (!result.success) throw new Error(result.message || '登录失败');
    location.href = '/admin';
  } catch (error) {
    elements.adminError.textContent = error.message;
    elements.adminError.classList.remove('hidden');
  }
}

async function logout() {
  try { await fetch('/api/logout', { method: 'POST', credentials: 'same-origin' }); }
  finally {
    sessionStorage.setItem('ycloud-stay-signed-out', '1');
    location.replace('/');
  }
}

elements.uploadButton.addEventListener('click', () => {
  if (!state.canWrite) showAdmin();
  else elements.fileInput.click();
});
elements.newFolderButton.addEventListener('click', () => openFolderModal());
elements.fileInput.addEventListener('change', event => uploadFiles(event.target.files));
elements.uploadCloseButton.addEventListener('click', () => {
  if (!state.uploading) hideModal(elements.uploadModal);
});
elements.createFolderButton.addEventListener('click', createFolder);
elements.folderName.addEventListener('keydown', event => { if (event.key === 'Enter') createFolder(); });
elements.selectAllButton.addEventListener('click', event => {
  event.stopPropagation();
  toggleSelectAll();
});
elements.searchInput.addEventListener('input', () => {
  clearTimeout(elements.searchInput.renderTimer);
  elements.searchInput.renderTimer = setTimeout(() => {
    state.query = elements.searchInput.value;
    state.selected.clear();
    renderFiles();
  }, 80);
});
elements.pickerConfirmButton.addEventListener('click', () => {
  const callback = state.pickerCallback;
  hideModal(elements.pickerModal);
  state.pickerCallback = null;
  if (callback) callback(state.pickerPath);
});
elements.adminButton.addEventListener('click', showAdmin);
elements.adminLoginButton.addEventListener('click', adminLogin);
elements.adminPass.addEventListener('keydown', event => { if (event.key === 'Enter') adminLogin(); });
elements.logoutButton.addEventListener('click', logout);
elements.backToTopButton.addEventListener('click', () => window.scrollTo({ top: 0, behavior: 'smooth' }));
function updateBackToTop() {
  elements.backToTopButton.classList.toggle('visible', isMobileLayout() && window.scrollY > 360);
}
window.addEventListener('scroll', updateBackToTop, { passive: true });
document.querySelectorAll('[data-sort]').forEach(button => button.addEventListener('click', () => {
  const next = button.dataset.sort;
  state.ascending = state.sort === next ? !state.ascending : true;
  state.sort = next;
  renderFiles();
}));
document.querySelectorAll('[data-close]').forEach(button => button.addEventListener('click', () => {
  const modal = document.getElementById(button.dataset.close);
  if (modal) hideModal(modal);
}));
document.addEventListener('click', event => {
  if (isMobileLayout() && state.selected.size > 0 && event.target.closest('.file-row, .context-menu, .back-to-top')) return;
  hideContextMenu();
});
document.addEventListener('contextmenu', event => {
  if (event.target.closest('.context-menu')) return;
  if (!event.target.closest('.file-panel')) return;
  if (isMobileLayout()) {
    event.preventDefault();
    return;
  }
  if (!event.target.closest('.file-row')) showContextMenu(event);
});
window.matchMedia('(max-width: 760px)').addEventListener('change', () => {
  syncMobileSelectionMenu();
  updateBackToTop();
});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape') {
    document.querySelectorAll('.overlay.active').forEach(modal => {
      if (modal !== elements.uploadModal || !state.uploading) hideModal(modal);
    });
    state.selected.clear();
    renderFiles();
  }
});

updateBackToTop();
refresh();
