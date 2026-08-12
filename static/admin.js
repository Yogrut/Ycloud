'use strict';

const MASK = '••••••';
const state = {
  shares: [], locks: [], shareId: null, lockId: null,
  hasGlobalWebPassword: false
};
const byId = id => document.getElementById(id);

async function request(url, options = {}) {
  const response = await fetch(url, { credentials: 'same-origin', ...options });
  if (response.status === 401 || response.status === 403) {
    location.replace('/browse');
    throw new Error('管理员登录已失效');
  }
  if (!response.ok) {
    let message = `请求失败 (${response.status})`;
    try {
      const data = await response.json();
      message = data?.error?.message || data?.message || message;
    } catch (_) { /* response has no JSON body */ }
    throw new Error(message);
  }
  return response.status === 204 ? null : response.json();
}

function toast(message, danger = false) {
  const element = byId('toast');
  element.textContent = message;
  element.classList.toggle('danger', danger);
  element.classList.add('show');
  clearTimeout(element.timer);
  element.timer = setTimeout(() => element.classList.remove('show'), 2800);
}

function normalizePath(value) {
  return String(value || '').replace(/\\/g, '/').split('/').filter(Boolean).join('/');
}

function displayPath(value) {
  const normalized = normalizePath(value);
  return normalized ? `/${normalized}` : '/';
}

function setFormError(id, message = '') {
  const element = byId(id);
  element.textContent = message;
  element.classList.toggle('hidden', !message);
}

function button(label, className, action) {
  const element = document.createElement('button');
  element.type = 'button';
  element.className = `btn ${className}`;
  element.textContent = label;
  element.addEventListener('click', action);
  return element;
}

function tag(label, className = '') {
  const element = document.createElement('span');
  element.className = `tag ${className}`.trim();
  element.textContent = label;
  return element;
}

function renderShares() {
  const list = byId('list');
  list.replaceChildren();
  if (!state.shares.length) {
    const empty = document.createElement('div');
    empty.className = 'empty-card';
    empty.textContent = '暂无 WebDAV 挂载';
    list.append(empty);
    return;
  }
  for (const share of state.shares) {
    const row = document.createElement('div');
    row.className = 'item';
    const info = document.createElement('div');
    info.className = 'item-info';
    const name = document.createElement('div');
    name.className = 'item-name';
    name.textContent = share.name;
    const meta = document.createElement('div');
    meta.className = 'item-meta';
    meta.textContent = `${displayPath(share.path)} · /dav/${share.name} · 用户 ${share.username || '未设置'}`;
    info.append(name, meta);
    const tags = document.createElement('div');
    tags.className = 'tags';
    tags.append(tag(share.webdav_enabled ? 'WebDAV 已启用' : 'WebDAV 已停用', share.webdav_enabled ? 'on' : ''));
    if (share.has_password) tags.append(tag('密码已设置', 'on'));
    tags.append(tag(share.readonly ? '只读' : '读写', share.readonly ? 'readonly' : ''));
    const actions = document.createElement('div');
    actions.className = 'item-actions';
    actions.append(
      button('编辑', 'btn-secondary', () => openShare(share)),
      button('删除', 'btn-danger', () => deleteShare(share))
    );
    row.append(info, tags, actions);
    list.append(row);
  }
}

function openShare(share = null) {
  state.shareId = share?.id || null;
  byId('dlgTitle').textContent = share ? '编辑 WebDAV 挂载' : '新建 WebDAV 挂载';
  byId('dlgBtn').textContent = share ? '保存' : '创建';
  byId('dlgName').value = share?.name || '';
  byId('dlgPath').value = displayPath(share?.path || '');
  byId('dlgUser').value = share?.username || '';
  byId('dlgPwd').value = share?.has_password ? MASK : '';
  byId('dlgDav').checked = share?.webdav_enabled ?? true;
  byId('dlgRO').checked = share?.readonly ?? false;
  setFormError('shareError');
  byId('dlg').classList.add('active');
  byId('dlgName').focus();
}

async function saveShare(event) {
  event.preventDefault();
  const editing = Boolean(state.shareId);
  const password = byId('dlgPwd').value;
  const body = {
    name: byId('dlgName').value.trim(),
    path: normalizePath(byId('dlgPath').value),
    username: byId('dlgUser').value.trim(),
    webdav_enabled: byId('dlgDav').checked,
    readonly: byId('dlgRO').checked
  };
  if (!editing || password !== MASK) body.password = password;
  const hasUsablePassword = editing ? (password === MASK || password.length > 0) : password.length > 0;
  if (body.webdav_enabled && (!body.username || !hasUsablePassword)) {
    setFormError('shareError', '启用 WebDAV 必须设置用户名和密码。');
    return;
  }
  try {
    await request(editing ? `/api/admin/shares/${encodeURIComponent(state.shareId)}` : '/api/admin/shares', {
      method: editing ? 'PUT' : 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    });
    byId('dlg').classList.remove('active');
    await load(false);
    toast(editing ? 'WebDAV 挂载已更新' : 'WebDAV 挂载已创建');
  } catch (error) { setFormError('shareError', error.message); }
}

async function deleteShare(share) {
  if (!await window._spConfirm(`删除 WebDAV 挂载“${share.name}”？磁盘中的文件不会被删除。`)) return;
  try {
    await request(`/api/admin/shares/${encodeURIComponent(share.id)}`, { method: 'DELETE' });
    await load(false);
    toast('WebDAV 挂载已删除');
  } catch (error) { toast(error.message, true); }
}

function renderLocks() {
  const list = byId('lockList');
  list.replaceChildren();
  if (!state.locks.length) {
    const empty = document.createElement('div');
    empty.className = 'empty-card';
    empty.textContent = '暂无网页文件夹锁';
    list.append(empty);
    return;
  }
  for (const lock of state.locks) {
    const row = document.createElement('div');
    row.className = 'item';
    const info = document.createElement('div');
    info.className = 'item-info';
    const name = document.createElement('div');
    name.className = 'item-name';
    name.textContent = displayPath(lock.path);
    const meta = document.createElement('div');
    meta.className = 'item-meta';
    meta.textContent = '保护该网页目录及其全部子目录';
    info.append(name, meta);
    const actions = document.createElement('div');
    actions.className = 'item-actions';
    actions.append(
      button('编辑', 'btn-secondary', () => openLock(lock)),
      button('删除', 'btn-danger', () => deleteLock(lock))
    );
    row.append(info, tag('密码已设置', 'on'), actions);
    list.append(row);
  }
}

function openLock(lock = null) {
  state.lockId = lock?.id || null;
  byId('lockDlgTitle').textContent = lock ? '编辑文件夹锁' : '新建文件夹锁';
  byId('lockDlgBtn').textContent = lock ? '保存' : '创建';
  byId('lockDlgPath').value = displayPath(lock?.path || '');
  byId('lockDlgPwd').value = lock ? MASK : '';
  setFormError('lockError');
  byId('lockDlg').classList.add('active');
  byId('lockDlgPath').focus();
}

async function saveLock(event) {
  event.preventDefault();
  const editing = Boolean(state.lockId);
  const password = byId('lockDlgPwd').value;
  const body = { path: normalizePath(byId('lockDlgPath').value) };
  if (!editing || password !== MASK) body.password = password;
  if (!body.path) return setFormError('lockError', '不能给存储根目录加锁，请选择具体文件夹。');
  if (!password) return setFormError('lockError', '文件夹锁必须有密码；若不需要保护，请删除该锁。');
  try {
    await request(editing ? `/api/admin/locks/${encodeURIComponent(state.lockId)}` : '/api/admin/locks', {
      method: editing ? 'PUT' : 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    });
    byId('lockDlg').classList.remove('active');
    await load(false);
    toast(editing ? '文件夹锁已更新' : '文件夹锁已创建');
  } catch (error) { setFormError('lockError', error.message); }
}

async function deleteLock(lock) {
  if (!await window._spConfirm(`删除 ${displayPath(lock.path)} 的文件夹锁？`)) return;
  try {
    await request(`/api/admin/locks/${encodeURIComponent(lock.id)}`, { method: 'DELETE' });
    await load(false);
    toast('文件夹锁已删除');
  } catch (error) { toast(error.message, true); }
}

async function saveAccount() {
  const body = { username: byId('adminUser').value.trim() };
  const adminPassword = byId('adminPass').value;
  const webPassword = byId('globalWebPass').value;
  const adminPasswordChanged = adminPassword !== MASK;
  if (adminPasswordChanged) {
    if (!adminPassword) return toast('管理员密码不能为空', true);
    body.password = adminPassword;
  }
  if (webPassword !== MASK) body.global_web_password = webPassword;
  if (!webPassword && state.hasGlobalWebPassword) {
    const confirmed = await window._spConfirm('移除网页访问密码后，任何能连接服务器的人都可进入文件浏览界面。文件夹锁仍然有效。是否继续？');
    if (!confirmed) return;
  }
  try {
    await request('/api/admin/account', {
      method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body)
    });
    toast(adminPasswordChanged ? '账户已更新，请重新登录' : '账户设置已更新');
    if (adminPasswordChanged) setTimeout(() => location.replace('/browse'), 700);
    else await load(true);
  } catch (error) { toast(error.message, true); }
}

async function load(resetAccount = true) {
  const info = await request('/api/admin/info');
  state.shares = info.shares || [];
  state.locks = info.folder_locks || [];
  state.hasGlobalWebPassword = Boolean(info.has_global_web_password);
  if (resetAccount) {
    byId('adminUser').value = info.username;
    byId('adminPass').value = MASK;
    byId('globalWebPass').value = state.hasGlobalWebPassword ? MASK : '';
  }
  renderShares();
  renderLocks();
}

byId('newShare').addEventListener('click', () => openShare());
byId('cancelShare').addEventListener('click', () => byId('dlg').classList.remove('active'));
byId('shareForm').addEventListener('submit', saveShare);
byId('newLock').addEventListener('click', () => openLock());
byId('cancelLock').addEventListener('click', () => byId('lockDlg').classList.remove('active'));
byId('lockForm').addEventListener('submit', saveLock);
byId('saveAccount').addEventListener('click', saveAccount);
for (const id of ['dlgPwd', 'lockDlgPwd', 'adminPass', 'globalWebPass']) {
  byId(id).addEventListener('focus', event => {
    if (event.target.value === MASK) event.target.select();
  });
}
for (const overlay of [byId('dlg'), byId('lockDlg')]) {
  overlay.addEventListener('click', event => {
    if (event.target === overlay) overlay.classList.remove('active');
  });
}
load().catch(error => {
  byId('list').textContent = `加载失败：${error.message}`;
  byId('lockList').replaceChildren();
});
