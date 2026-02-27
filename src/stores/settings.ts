import { defineStore } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import type { Settings } from '../types';
import { setI18nLanguage } from '../i18n';

export const useSettingsStore = defineStore('settings', {
  state: (): Settings => ({
    theme: 'dark',
    language: 'zh',
    ai: {
      apiUrl: 'https://api.openai.com/v1',
      apiKey: '',
      modelName: 'gpt-3.5-turbo'
    },
    terminalAppearance: {
      fontSize: 14,
      fontFamily: 'Menlo, Monaco, "Courier New", monospace',
      cursorStyle: 'block',
      lineHeight: 1.0
    },
    fileManager: {
      viewMode: 'flat',
      layout: 'bottom',
      sftpBufferSize: 512
    },
    sshPool: {
      maxBackgroundSessions: 3,
      enableAutoCleanup: true,
      cleanupIntervalMinutes: 5
    },
    connectionTimeout: {
      connectionTimeoutSecs: 15,
      jumpHostTimeoutSecs: 30,
      localForwardTimeoutSecs: 10,
      commandTimeoutSecs: 30,
      sftpOperationTimeoutSecs: 60
    },
    reconnect: {
      maxReconnectAttempts: 5,
      initialDelayMs: 1000,
      maxDelayMs: 30000,
      backoffMultiplier: 2.0,
      enableAutoReconnect: true
    },
    heartbeat: {
      tcpKeepaliveIntervalSecs: 60,
      sshKeepaliveIntervalSecs: 15,
      appHeartbeatIntervalSecs: 30,
      heartbeatTimeoutSecs: 5,
      failedHeartbeatsBeforeAction: 3
    },
    poolHealth: {
      healthCheckIntervalSecs: 60,
      sessionWarmupCount: 1,
      maxSessionAgeMinutes: 60,
      unhealthyThreshold: 3
    },
    networkAdaptive: {
      enableAdaptive: true,
      latencyCheckIntervalSecs: 30,
      highLatencyThresholdMs: 300,
      lowBandwidthThresholdKbps: 100
    }
  }),
  actions: {
    async loadSettings() {
      try {
        const settings = await invoke<Settings>('get_settings');
        this.$patch(settings);
        this.applyTheme();
        await this.applyLanguage();
      } catch (e) {
        console.error('Failed to load settings', e);
      }
    },
    async saveSettings(settings: Partial<Settings>) {
      this.$patch(settings);
      this.applyTheme();
      if (settings.language) {
        await this.applyLanguage();
      }
      try {
        await invoke('save_settings', { settings: this.$state });
      } catch (e) {
        console.error('Failed to save settings', e);
      }
    },
    applyTheme() {
      if (this.theme === 'dark') {
        document.documentElement.classList.add('dark');
      } else {
        document.documentElement.classList.remove('dark');
      }
    },
    async applyLanguage() {
      await setI18nLanguage(this.language);
    }
  }
});
