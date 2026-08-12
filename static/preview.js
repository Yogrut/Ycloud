'use strict';

const content = document.getElementById('content');
const requestPath = new URLSearchParams(location.search).get('path') || '';
const name = requestPath.split('/').filter(Boolean).pop() || '';
const extension = name.includes('.') ? name.split('.').pop().toLowerCase() : '';
const previewUrl = `/api/preview?path=${encodeURIComponent(`/${requestPath.replace(/^\/+/, '')}`)}`;
const downloadUrl = `/api/download?path=${encodeURIComponent(`/${requestPath.replace(/^\/+/, '')}`)}`;

document.getElementById('fileName').textContent = name || '文件预览';
document.getElementById('download').addEventListener('click', () => { location.href = downloadUrl; });
document.getElementById('close').addEventListener('click', () => window.close());
if (name) {
  document.title = `${name} - 文件预览`;
  render();
} else {
  showMessage('缺少文件路径', '请返回文件浏览器后重新打开预览。', false);
}

function media(tag) {
  const element = document.createElement(tag);
  element.src = previewUrl;
  if (tag === 'img') element.alt = name;
  else element.controls = true;
  content.replaceChildren(element);
}

async function textPreview() {
  try {
    const response = await fetch(previewUrl, { credentials: 'same-origin' });
    if (!response.ok) throw new Error(`预览失败 (${response.status})`);
    const text = await response.text();
    const pre = document.createElement('pre');
    pre.textContent = text;
    content.replaceChildren(pre);
  } catch (error) {
    showMessage(name, error.message, true);
  }
}

function showMessage(title, message, downloadable) {
  const box = document.createElement('div');
  box.className = 'message';
  const heading = document.createElement('strong');
  heading.textContent = title;
  const detail = document.createElement('p');
  detail.textContent = message;
  box.append(heading, detail);
  if (downloadable) {
    const link = document.createElement('a');
    link.className = 'download btn btn-primary';
    link.href = downloadUrl;
    link.textContent = '下载文件';
    box.append(link);
  }
  content.replaceChildren(box);
}

function render() {
  const images = new Set(['png','jpg','jpeg','gif','webp','bmp','ico','avif']);
  const videos = new Set(['mp4','webm','mov']);
  const audio = new Set(['mp3','wav','ogg','flac','aac','m4a']);
  const text = new Set(['txt','md','markdown','rs','py','js','ts','go','java','c','cpp','h','html','css','json','xml','yaml','yml','toml','sh','sql','vue','svelte','rb','php','swift','kt','cs','lua','log','csv']);
  if (images.has(extension)) return media('img');
  if (videos.has(extension)) return media('video');
  if (audio.has(extension)) return media('audio');
  if (extension === 'pdf') {
    const frame = document.createElement('iframe');
    frame.src = previewUrl;
    frame.title = name;
    content.replaceChildren(frame);
    return;
  }
  if (text.has(extension)) return textPreview();
  showMessage(name, '此文件类型不在安全预览列表中。', true);
}
