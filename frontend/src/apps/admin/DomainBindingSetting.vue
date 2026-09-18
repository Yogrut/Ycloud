<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import { computed, ref, watch } from 'vue'
import { getDomainBinding, removeDomainBinding, saveDomainBinding, type DomainBindingView } from '../../shared/api/admin'
import SettingRow from '../../shared/components/SettingRow.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ initial?: DomainBindingView }>()
const locale = useLocale()
const feedbackRevision = ref(0)
const status = ref<DomainBindingView>(props.initial ?? { binding: null, source: 'none' })
const open = ref(false)
const busy = ref(false)
const url = ref('')
const proxies = ref('')
const error = ref('')
const message = ref('')
const removing = ref(false)
const label = computed(() => locale.text('域名绑定', 'Domain binding'))
const summary = computed(() => status.value.binding?.public_url ?? locale.text('未设置', 'Not set'))

watch(() => props.initial, value => { if (value && !busy.value) status.value = value })

async function refresh(): Promise<void> {
  try { status.value = await getDomainBinding() }
  catch (reason) { error.value = reason instanceof Error ? reason.message : locale.text('无法读取域名设置', 'Unable to load domain settings') }
}

function syncDraft(): void {
  url.value = status.value.binding?.public_url ?? ''
  proxies.value = status.value.binding?.trusted_proxy_ips.join(', ') ?? ''
}

async function show(): Promise<void> {
  open.value = true
  error.value = ''; message.value = ''; removing.value = false
  busy.value = true
  await refresh()
  syncDraft()
  busy.value = false
}

async function submit(): Promise<void> {
  if (busy.value) return
  busy.value = true; error.value = ''; message.value = ''
  try {
    if (removing.value) {
      status.value = await removeDomainBinding()
      removing.value = false
      syncDraft()
      message.value = locale.text('已解除后台绑定，恢复部署配置；请使用部署时的访问地址。', 'Binding removed. Use your deployment address; deployment settings apply again.')
    } else {
      status.value = await saveDomainBinding({ public_url: url.value.trim(), trusted_proxy_ips: proxies.value.split(/[\s,，]+/).filter(Boolean) })
      syncDraft()
      message.value = locale.text('域名绑定已立即生效，请使用绑定域名访问。', 'Domain binding is active. Use the bound domain to access Ycloud.')
    }
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : locale.text('保存失败', 'Unable to save changes')
  } finally { busy.value = false }
}

function cancel(): void {
  if (!busy.value) open.value = false
}
</script>

<template>
  <SettingRow :label="label" :value="summary" @edit="show" />
  <SettingsDrawer v-if="open" :title="label" :busy="busy" @close="open = false">
    <form class="drawer-form domain-binding-form" @submit.prevent="feedbackRevision++; submit()">
      <template v-if="removing">
        <p class="domain-notice confirmation-warning">{{ locale.text('确认解除域名绑定？解除后恢复部署时的访问配置，当前地址可能立即无法访问。', 'Remove domain binding? Deployment access settings will be restored and this address may stop working immediately.') }}</p>
        <p>{{ locale.text('请先确认你知道部署时的内网访问地址；若部署变量已配置域名，则恢复该域名。', 'Make sure you know the original LAN address. If deployment variables specify a domain, that domain will be restored.') }}</p>
      </template>
      <template v-else>
        <label>{{ locale.text('访问地址', 'Public address') }}<input v-model="url" class="input" type="url" required maxlength="300" placeholder="https://cloud.example.com" autocomplete="off"></label>
        <p class="field-hint">{{ locale.text('仅支持 HTTPS 域名，可带端口。确认后文件页面、后台及 WebDAV 均使用该域名，内网 IP 直连将关闭。', 'Use an HTTPS domain, optionally with a port. After confirmation, files, admin and WebDAV use this domain; direct LAN IP access is disabled.') }}</p>
        <label>{{ locale.text('可信反向代理 IP', 'Trusted proxy IPs') }}<input v-model="proxies" class="input" required maxlength="720" placeholder="172.18.0.1" autocomplete="off"></label>
        <p class="field-hint">{{ locale.text('填写 Ycloud 实际收到连接的代理 IP，多个用逗号分隔，不是访客 IP。代理须传递原始 Host、单值 X-Forwarded-For 和 X-Forwarded-Proto: https。', 'Enter the proxy peer IP seen by Ycloud, not visitor IPs; separate multiple addresses with commas. Forward the original Host, one X-Forwarded-For IP, and X-Forwarded-Proto: https.') }}</p>
        <p class="field-hint">{{ locale.text('请先完成 DNS 解析、证书和反向代理配置。点击确认后立即生效，请核对地址及代理 IP；填写错误可能导致无法访问。', 'Configure DNS, a certificate and the reverse proxy first. Changes apply immediately on confirmation; incorrect addresses or proxy IPs may prevent access.') }}</p>
        <p v-if="status.source === 'environment'" class="field-hint">{{ locale.text('当前配置来自部署变量。新绑定确认后优先生效，无需修改容器变量。', 'Current settings come from deployment variables. A confirmed binding takes precedence without changing those variables.') }}</p>
        <button v-if="status.source === 'settings'" class="domain-remove" type="button" @click="removing = true; error = ''; message = ''">{{ locale.text('解除域名绑定', 'Remove domain binding') }}</button>
      </template>
      <AppFeedback :revision="feedbackRevision" :message="message" kind="success" />
      <a v-if="message && status.binding" class="domain-open-link" :href="`${status.binding.public_url}/admin/account`" target="_blank" rel="noopener noreferrer">{{ locale.text('打开绑定域名', 'Open bound domain') }}</a>
      <AppFeedback :revision="feedbackRevision" :message="error" />
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="busy" @click="cancel">{{ locale.text('取消', 'Cancel') }}</button>
        <button class="btn" type="submit" :disabled="busy">{{ locale.text('确认', 'Confirm') }}</button>
      </div>
    </form>
  </SettingsDrawer>
</template>

<style scoped>
.domain-binding-form { font-size: 14px; line-height: 1.6; }
.domain-binding-form p { margin: 0; }
.domain-open-link { color: var(--accent); }
.domain-remove { align-self: flex-start; padding: 0; border: 0; background: transparent; color: var(--danger); cursor: pointer; font-size: 14px; }
</style>
