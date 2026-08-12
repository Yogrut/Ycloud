'use strict';

const state = {
  path: '',
  entries: [],
  selected: new Set(),
  query: '',
  sort: 'name',
  ascending: true,
  canWrite: true,
  maxUploadBytes: 0,
  pickerPath: '',
  pickerCallback: null,
  mkdirTarget: '',
  contextPath: '',
  contextIsDirectory: false
};

const elements = Object.fromEntries([
  'adminButton','logoutButton','uploadButton','newFolderButton','searchInput',
  'limitNote','breadcrumb','selectAllButton','fileList',
  'emptyState','fileInput','toast','contextMenu','folderModal','folderName','folderError',
  'createFolderButton','pickerModal','pickerTitle','pickerPath','pickerList',
  'pickerConfirmButton','adminModal','adminUser','adminPass','adminError','adminLoginButton'
].map(id => [id, document.getElementById(id)]));

async function request(url, options = {}) {
  const response = await fetch(url, { credentials: 'same-origin', ...options });
  if (response.status === 401) {
    location.href = '/';
    throw new Error('登录已失效');
  }
  const type = response.headers.get('content-type') || '';
  const body = type.includes('application/json') ? await response.json() : null;
  if (!response.ok) {
    throw new Error(body?.error?.message || body?.message || `请求失败 (${response.status})`);
  }
  return body;
}

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

function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

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
  root.addEventListener('click', () => navigate(''));
  elements.breadcrumb.append(root);
  let accumulated = '';
  for (const part of state.path.split('/').filter(Boolean)) {
    accumulated = accumulated ? `${accumulated}/${part}` : part;
    const destination = accumulated;
    const button = document.createElement('button');
    button.className = 'crumb';
    button.type = 'button';
    button.textContent = `${part}/`;
    button.addEventListener('click', () => navigate(destination));
    elements.breadcrumb.append(button);
  }
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
  row.addEventListener('click', () => toggleSelection(entry.path));
  row.addEventListener('dblclick', () => openEntry(entry));
  row.addEventListener('contextmenu', event => showContextMenu(event, entry));
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
    control.disabled = !state.canWrite;
    control.title = state.canWrite ? '' : '当前共享为只读';
  }
}

async function refresh() {
  try {
    const data = await request(fileApi());
    state.path = (data.current_path || '').replace(/^\/+|\/+$/g, '');
    state.entries = data.entries || [];
    state.canWrite = data.can_write !== false;
    state.maxUploadBytes = Number(data.max_upload_bytes || 0);
    state.selected.clear();
    elements.limitNote.textContent = data.truncated
      ? '目录内容超过服务器显示上限，当前仅显示部分项目'
      : (state.maxUploadBytes ? `单次上传上限 ${formatSize(state.maxUploadBytes)}` : '');
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

function updateSelectionControls() {
  const entries = visibleEntries();
  const allSelected = entries.length > 0 && entries.every(entry => state.selected.has(entry.path));
  elements.selectAllButton.parentElement?.classList.toggle('selected', allSelected);
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

async function uploadFiles(files) {
  if (!state.canWrite || !files.length) return;
  const total = [...files].reduce((sum, file) => sum + file.size, 0);
  if (state.maxUploadBytes && total > state.maxUploadBytes) {
    return showToast(`所选文件总大小超过 ${formatSize(state.maxUploadBytes)}`);
  }
  const form = new FormData();
  [...files].forEach(file => form.append('file', file));
  elements.uploadButton.disabled = true;
  try {
    const result = await request(actionApi('upload'), { method: 'POST', body: form });
    showToast(`已上传 ${result.uploaded?.length || files.length} 个文件`);
    await refresh();
  } catch (error) { showToast(error.message); }
  finally { elements.uploadButton.disabled = !state.canWrite; elements.fileInput.value = ''; }
}

function openFolderModal(target = state.path) {
  if (!state.canWrite) return;
  state.mkdirTarget = target;
  elements.folderName.value = '';
  elements.folderError.classList.add('hidden');
  showModal(elements.folderModal);
  elements.folderName.focus();
}

async function createFolder() {
  const name = elements.folderName.value.trim();
  if (!name) return;
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
    await request('/api/batch/delete', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ paths })
    });
    showToast(`已删除 ${paths.length} 个项目`);
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
      await request(`/api/batch/${operation}`, {
        method: operation === 'move' ? 'PUT' : 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ paths, target: '/' + target })
      });
      showToast(operation === 'move' ? '移动成功' : '复制成功');
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
  elements.contextMenu.append(button);
}

function addMenuSeparator() {
  const separator = document.createElement('div');
  separator.className = 'menu-separator';
  elements.contextMenu.append(separator);
}

function addMenuCaption(count) {
  const caption = document.createElement('div');
  caption.className = 'menu-caption';
  caption.textContent = `已选择 ${count} 项`;
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
      if (state.canWrite) {
        addMenuItem('移动', 'move', () => transfer('move', paths));
        addMenuItem('复制', 'copy', () => transfer('copy', paths));
        addMenuSeparator();
        addMenuItem('删除', 'trash', () => deletePaths(paths), true);
      }
    } else {
      if (entry.is_dir) addMenuItem('打开', 'open', () => openEntry(entry));
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
  const menuRect = elements.contextMenu.getBoundingClientRect();
  elements.contextMenu.style.left = `${Math.max(8, Math.min(event.clientX, innerWidth - menuRect.width - 8))}px`;
  elements.contextMenu.style.top = `${Math.max(8, Math.min(event.clientY, innerHeight - menuRect.height - 8))}px`;
}

function hideContextMenu() { elements.contextMenu.classList.remove('active'); }

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

elements.uploadButton.addEventListener('click', () => elements.fileInput.click());
elements.newFolderButton.addEventListener('click', () => openFolderModal());
elements.fileInput.addEventListener('change', event => uploadFiles(event.target.files));
elements.createFolderButton.addEventListener('click', createFolder);
elements.folderName.addEventListener('keydown', event => { if (event.key === 'Enter') createFolder(); });
elements.selectAllButton.addEventListener('click', toggleSelectAll);
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
document.addEventListener('click', hideContextMenu);
document.addEventListener('contextmenu', event => {
  if (event.target.closest('.context-menu')) return;
  if (event.target.closest('.file-panel') && !event.target.closest('.file-row')) showContextMenu(event);
});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape') {
    document.querySelectorAll('.overlay.active').forEach(hideModal);
    state.selected.clear();
    renderFiles();
  }
});

refresh();
