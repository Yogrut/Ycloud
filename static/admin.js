'use strict';

const MASK = '••••••';
const state = {
  shares: [], locks: [], shareId: null, lockId: null,
  hasGlobalWebPassword: false, loginSecurity: [],
  maxUploadBytes: 0, maxArchiveBytes: 0, maxArchiveEntries: 0
};
const byId = id => document.getElementById(id);

const ADMIN_SECTIONS = new Set(['webdav', 'locks', 'limits', 'account', 'security']);
const MIGRATED_ADMIN_ROUTES = new Map([
  ['locks', '/v2/admin/locks'],
  ['limits', '/v2/admin/limits'],
  ['account', '/v2/admin/account'],
  ['security', '/v2/admin/security']
]);
const candidateReturn = new URLSearchParams(location.search).get('return') === 'v2' || location.port === '5173';
const GIB = 1024 ** 3;

function activateSection(section, updateHistory = true) {
  const selected = ADMIN_SECTIONS.has(section) ? section : 'webdav';
  if (candidateReturn && MIGRATED_ADMIN_ROUTES.has(selected)) {
    location.replace(MIGRATED_ADMIN_ROUTES.get(selected));
    return;
  }
  for (const item of document.querySelectorAll('[data-admin-section]')) {
    const active = item.dataset.adminSection === selected;
    item.classList.toggle('active', active);
    if (active) item.setAttribute('aria-current', 'page');
    else item.removeAttribute('aria-current');
  }
  for (const panel of document.querySelectorAll('[data-admin-panel]')) {
    panel.hidden = panel.dataset.adminPanel !== selected;
  }
  if (updateHistory && location.hash !== `#${selected}`) {
    history.replaceState(null, '', `#${selected}`);
  }
}

async function request(url, options = {}) {
  const response = await fetch(url, { credentials: 'same-origin', ...options });
  if (response.status === 401 || response.status === 403) {
    location.replace(candidateReturn ? '/v2/browse' : '/browse');
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

function bytesToGiB(value) {
  return Number((Number(value || 0) / GIB).toFixed(3));
}

function gibToBytes(value) {
  return Math.round(Number(value) * GIB);
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
  if (password !== MASK && password.length > 0 && [...password].length < 12) {
    setFormError('shareError', 'WebDAV 密码至少需要 12 位。');
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

function entryLabel(entry) {
  return ({ admin: '管理员', web: '首页', web_dav: 'WebDAV' })[entry] || entry;
}

function formatSecurityTime(value) {
  return value ? new Date(value * 1000).toLocaleString() : '—';
}

function renderLoginSecurity() {
  const list = byId('securityList');
  list.replaceChildren();
  if (!state.loginSecurity.length) {
    const empty = document.createElement('div');
    empty.className = 'empty-card';
    empty.textContent = '暂无登录安全记录';
    list.append(empty);
    return;
  }
  const now = Math.floor(Date.now() / 1000);
  for (const record of state.loginSecurity) {
    const row = document.createElement('div');
    row.className = 'item security-item';
    const info = document.createElement('div');
    info.className = 'item-info';
    const name = document.createElement('div');
    name.className = 'item-name security-ip';
    name.textContent = `${record.ip} · ${entryLabel(record.entry)}`;
    const meta = document.createElement('div');
    meta.className = 'item-meta security-meta';
    const blocked = record.blocked_until && record.blocked_until > now;
    meta.textContent = `结果：${record.last_result} · 失败 ${record.failed_attempts} 次 · 最后尝试 ${formatSecurityTime(record.last_attempt_at)} · 最后成功 ${formatSecurityTime(record.last_success_at)}${blocked ? ` · 限制至 ${formatSecurityTime(record.blocked_until)}` : ''}`;
    if (record.user_agent) {
      const agent = document.createElement('div');
      agent.className = 'item-meta security-agent';
      agent.textContent = record.user_agent;
      info.append(name, meta, agent);
    } else info.append(name, meta);
    const actions = document.createElement('div');
    actions.className = 'item-actions';
    if (record.failed_attempts || blocked) {
      actions.append(button('解除限制', 'btn-secondary', () => unblockLogin(record)));
    }
    row.append(info, tag(blocked ? '已限制' : '正常', blocked ? '' : 'on'), actions);
    list.append(row);
  }
}

async function unblockLogin(record) {
  if (!await window._spConfirm(`解除 ${record.ip} 的${entryLabel(record.entry)}登录限制？`)) return;
  try {
    await request('/api/admin/security/unblock', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ entry: record.entry, ip: record.ip })
    });
    await load(false);
    toast('已解除该 IP 的登录限制');
  } catch (error) { toast(error.message, true); }
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
  if (password !== MASK && [...password].length < 8) return setFormError('lockError', '文件夹锁密码至少需要 8 位。');
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
    if ([...adminPassword].length < 12) return toast('管理员密码至少需要 12 位', true);
    body.password = adminPassword;
  }
  if (webPassword !== MASK && webPassword && [...webPassword].length < 8) return toast('网页访问密码至少需要 8 位', true);
  if (webPassword !== MASK) body.global_web_password = webPassword;
  if (!webPassword && state.hasGlobalWebPassword) {
    const confirmed = await window._spConfirm('移除网页访问密码后，任何能连接服务器的人都可进入文件浏览界面。文件夹锁仍然有效。是否继续？');
    if (!confirmed) return;
  }
  try {
    const result = await request('/api/admin/account', {
      method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body)
    });
    toast(result.warning || (adminPasswordChanged ? '账户已更新，请重新登录' : '账户设置已更新'), Boolean(result.warning));
    if (adminPasswordChanged) setTimeout(() => location.replace('/browse'), 700);
    else await load(true);
  } catch (error) { toast(error.message, true); }
}

async function saveLimits() {
  const maxUploadBytes = gibToBytes(byId('maxUploadGiB').value);
  const maxArchiveBytes = gibToBytes(byId('maxArchiveGiB').value);
  const maxArchiveEntries = Number(byId('maxArchiveEntries').value);
  if (!Number.isSafeInteger(maxUploadBytes) || maxUploadBytes < 1024 ** 2 || maxUploadBytes > 100 * GIB) {
    return toast('单文件上传上限必须在 1 MiB 到 100 GiB 之间', true);
  }
  if (!Number.isSafeInteger(maxArchiveBytes) || maxArchiveBytes < 1024 ** 2 || maxArchiveBytes > 10 * GIB) {
    return toast('打包源文件总大小上限必须在 1 MiB 到 10 GiB 之间', true);
  }
  if (!Number.isInteger(maxArchiveEntries) || maxArchiveEntries < 1 || maxArchiveEntries > 5000) {
    return toast('打包条目数量上限必须在 1 到 5000 之间', true);
  }
  try {
    await request('/api/admin/limits', {
      method: 'PUT', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        max_upload_bytes: maxUploadBytes,
        max_archive_bytes: maxArchiveBytes,
        max_archive_entries: maxArchiveEntries
      })
    });
    await load(false);
    toast('传输限制已保存并立即生效');
  } catch (error) { toast(error.message, true); }
}

async function load(resetAccount = true) {
  const info = await request('/api/admin/info');
  state.shares = info.shares || [];
  state.locks = info.folder_locks || [];
  state.hasGlobalWebPassword = Boolean(info.has_global_web_password);
  state.loginSecurity = info.login_security || [];
  state.maxUploadBytes = Number(info.max_upload_bytes || 0);
  state.maxArchiveBytes = Number(info.max_archive_bytes || 0);
  state.maxArchiveEntries = Number(info.max_archive_entries || 0);
  byId('maxUploadGiB').value = bytesToGiB(state.maxUploadBytes);
  byId('maxArchiveGiB').value = bytesToGiB(state.maxArchiveBytes);
  byId('maxArchiveEntries').value = state.maxArchiveEntries;
  if (resetAccount) {
    byId('adminUser').value = info.username;
    byId('adminPass').value = MASK;
    byId('globalWebPass').value = state.hasGlobalWebPassword ? MASK : '';
  }
  renderShares();
  renderLocks();
  renderLoginSecurity();
}

byId('newShare').addEventListener('click', () => openShare());
if (candidateReturn) byId('returnFiles').href = '/v2/browse';
byId('cancelShare').addEventListener('click', () => byId('dlg').classList.remove('active'));
byId('shareForm').addEventListener('submit', saveShare);
byId('newLock').addEventListener('click', () => openLock());
byId('cancelLock').addEventListener('click', () => byId('lockDlg').classList.remove('active'));
byId('lockForm').addEventListener('submit', saveLock);
byId('saveAccount').addEventListener('click', saveAccount);
byId('saveLimits').addEventListener('click', saveLimits);
for (const item of document.querySelectorAll('[data-admin-section]')) {
  item.addEventListener('click', () => activateSection(item.dataset.adminSection));
}
window.addEventListener('hashchange', () => activateSection(location.hash.slice(1), false));
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
activateSection(location.hash.slice(1), false);
load().catch(error => {
  byId('list').textContent = `加载失败：${error.message}`;
  byId('lockList').replaceChildren();
  byId('securityList').replaceChildren();
});
