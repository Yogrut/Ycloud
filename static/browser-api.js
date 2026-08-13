export async function apiRequest(url, options = {}) {
  const response = await fetch(url, { credentials: 'same-origin', ...options });
  if (response.status === 401) {
    location.href = '/';
    throw new Error('登录已失效');
  }
  const type = response.headers.get('content-type') || '';
  const body = type.includes('application/json') ? await response.json() : null;
  if (!response.ok) {
    const partial = body?.failed ? `；失败 ${body.failed} 项` : '';
    throw new Error((body?.error?.message || body?.message || `请求失败 (${response.status})`) + partial);
  }
  return body;
}

export function rawUpload(url, file, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('PUT', url);
    xhr.withCredentials = true;
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.upload.addEventListener('progress', event => {
      if (event.lengthComputable) onProgress(event.loaded, event.total);
    });
    xhr.addEventListener('load', () => {
      if (xhr.status === 401) {
        location.href = '/';
        reject(new Error('登录已失效'));
        return;
      }
      let body = null;
      try { body = JSON.parse(xhr.responseText || 'null'); } catch (_) {}
      if (xhr.status >= 200 && xhr.status < 300) resolve(body);
      else reject(new Error(body?.error?.message || body?.message || `上传失败 (${xhr.status})`));
    });
    xhr.addEventListener('error', () => reject(new Error('网络连接中断')));
    xhr.addEventListener('abort', () => reject(new Error('上传已取消')));
    xhr.send(file);
  });
}
