'use strict';

const systemDialog = document.createElement('dialog');
systemDialog.className = 'system-dialog';
const systemBody = document.createElement('div');
systemBody.className = 'system-dialog__body';
const systemTitle = document.createElement('h3');
systemTitle.className = 'system-dialog__title';
const systemMessage = document.createElement('p');
systemMessage.className = 'system-dialog__message';
const systemInput = document.createElement('input');
systemInput.className = 'input system-dialog__input';
systemInput.autocomplete = 'off';
const systemActions = document.createElement('div');
systemActions.className = 'system-dialog__actions';
const systemCancel = document.createElement('button');
systemCancel.type = 'button';
systemCancel.className = 'btn btn-secondary';
systemCancel.textContent = '取消';
const systemConfirm = document.createElement('button');
systemConfirm.type = 'button';
systemConfirm.className = 'btn btn-primary';
systemConfirm.textContent = '确定';
systemActions.append(systemCancel, systemConfirm);
systemBody.append(systemTitle, systemMessage, systemActions);
systemDialog.append(systemBody);
document.body.append(systemDialog);

let settleSystemDialog = null;
let systemDialogUsesInput = false;

function closeSystemDialog(value) {
  if (systemDialog.open) systemDialog.close();
  const settle = settleSystemDialog;
  settleSystemDialog = null;
  if (settle) settle(value);
}

function openSystemDialog({ title, message, input = false, initial = '', inputType = 'text' }) {
  if (settleSystemDialog) closeSystemDialog(systemDialogUsesInput ? null : false);
  systemDialogUsesInput = input;
  systemTitle.textContent = title;
  systemMessage.textContent = message;
  systemInput.value = initial;
  systemInput.type = inputType;
  systemInput.autocomplete = inputType === 'password' ? 'current-password' : 'off';
  if (input) systemBody.insertBefore(systemInput, systemActions);
  else systemInput.remove();
  systemDialog.showModal();
  queueMicrotask(() => input ? systemInput.focus() : systemConfirm.focus());
  return new Promise(resolve => { settleSystemDialog = resolve; });
}

window._spConfirm = message => openSystemDialog({ title: '请确认', message });
window._spPrompt = (message, initial = '', inputType = 'text') =>
  openSystemDialog({ title: '请输入', message, input: true, initial, inputType });
systemCancel.addEventListener('click', () => closeSystemDialog(systemDialogUsesInput ? null : false));
systemConfirm.addEventListener('click', () => closeSystemDialog(systemDialogUsesInput ? systemInput.value : true));
systemDialog.addEventListener('cancel', event => {
  event.preventDefault();
  closeSystemDialog(systemDialogUsesInput ? null : false);
});
systemInput.addEventListener('keydown', event => {
  if (event.key === 'Enter') closeSystemDialog(systemInput.value);
});
