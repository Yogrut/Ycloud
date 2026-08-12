'use strict';

const form = document.getElementById('gateForm');
const password = document.getElementById('password');
const button = document.getElementById('enterButton');
const error = document.getElementById('error');

async function currentIdentity() {
  if (sessionStorage.getItem('ycloud-stay-signed-out') === '1') return;
  try {
    const response = await fetch('/api/me', { credentials: 'same-origin' });
    if (!response.ok) return;
    const identity = await response.json();
    if (identity.logged_in) location.replace('/browse');
    else if (identity.web_password_required === false) {
      const gate = await fetch('/api/gate', {
        method: 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: '' })
      });
      if (gate.ok) location.replace('/browse');
    }
  } catch (_) { /* login remains available while the service recovers */ }
}

form.addEventListener('submit', async event => {
  event.preventDefault();
  error.textContent = '';
  button.disabled = true;
  button.textContent = '验证中…';
  try {
    const response = await fetch('/api/gate', {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: password.value })
    });
    const data = await response.json().catch(() => ({}));
    if (!response.ok || !data.success) throw new Error(data?.error?.message || '密码错误');
    sessionStorage.removeItem('ycloud-stay-signed-out');
    location.replace('/browse');
  } catch (failure) {
    error.textContent = failure.message || '暂时无法连接服务';
    button.disabled = false;
    button.textContent = '进入';
    password.select();
  }
});

currentIdentity();
