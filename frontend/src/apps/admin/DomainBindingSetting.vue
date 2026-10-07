<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import { computed, ref } from 'vue'
import type { DomainBindingView } from '../../shared/api/admin'
import SettingRow from '../../shared/components/SettingRow.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import { useLocale } from '../../shared/i18n'
import { useDomainBinding } from './useDomainBinding'

const props = defineProps<{ initial?: DomainBindingView }>()
const locale = useLocale()
const feedbackRevision = ref(0)
const { status, open, busy, url, proxyIps, error, message, show, submit, cancel } = useDomainBinding(() => props.initial)
const label = computed(() => locale.text('域名绑定', 'Domain binding'))
const summary = computed(() => status.value.binding?.public_url ?? locale.text('未设置', 'Not set'))
</script>

<template>
  <SettingRow :label="label" :value="summary" @edit="show" />
  <SettingsDrawer v-if="open" :title="label" :busy="busy" @close="cancel">
    <form class="drawer-form domain-binding-form" @submit.prevent="feedbackRevision++; submit()">
      <label>{{ locale.text('访问地址', 'Public address') }}<input v-model="url" class="input" type="url" maxlength="300" placeholder="https://cloud.example.com" autocomplete="off"></label>
      <label>{{ locale.text('可信代理 IP', 'Trusted proxy IPs') }}<input v-model="proxyIps" class="input" type="text" maxlength="1024" placeholder="192.0.2.10" autocomplete="off" spellcheck="false"></label>
      <p class="field-hint">{{ locale.text('填写反代连接 Ycloud 时使用的 IP，不带协议或端口；多个 IP 用逗号分隔。留空不信任转发头，仅记录直连 IP。保存后立即生效。', 'Enter the IP used by the reverse proxy to connect to Ycloud, without a scheme or port. Separate multiple IPs with commas. Leave empty to ignore forwarded headers and record the direct peer. Applies immediately after saving.') }}</p>
      <p class="field-hint">{{ locale.text('仅支持 HTTPS 域名，可带端口。清空后确认即可解除绑定，恢复 HTTP 访问。域名解析、证书和转发由部署环境负责。', 'Use an HTTPS domain, optionally with a port. Clear and confirm to remove the binding and restore HTTP access. DNS, certificates and forwarding belong to the deployment environment.') }}</p>
      <p class="field-hint">{{ locale.text('绑定后文件页面、后台及 WebDAV 均校验该域名，并启用 HTTPS Cookie；内网 IP 直连将关闭。', 'When bound, files, admin and WebDAV validate this domain and use HTTPS cookies; direct LAN IP access is disabled.') }}</p>
      <AppFeedback :revision="feedbackRevision" :message="message" kind="success" />
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
</style>
