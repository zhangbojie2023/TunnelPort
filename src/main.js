const { invoke } = window.__TAURI__?.core ?? {};
const { listen } = window.__TAURI__?.event ?? {};

// ── Diagnostic logging ──
console.log("[PassPort] main.js loaded");
console.log("[PassPort] __TAURI__ available:", !!window.__TAURI__);
console.log("[PassPort] invoke available:", typeof invoke);
console.log("[PassPort] listen available:", typeof listen);
console.log("[PassPort] location.href:", location.href);

// ── State Machine ──
const STATES = {
  DISCONNECTED: 'disconnected',
  CONNECTING: 'connecting',
  CONNECTED: 'connected',
  ERROR: 'error',
};

let currentState = STATES.DISCONNECTED;
let errorMessage = null;
let reachability = null;

// Profile data
let allProfiles = [];
let activeProfileId = null;
let isNewProfilePending = false;

// ── Render: single source of UI truth ──
function render() {
  const ring = document.getElementById('statusRing');
  const label = document.getElementById('statusText');

  ring.className = 'status-ring';

  switch (currentState) {
    case STATES.CONNECTING:
      ring.classList.add('connecting');
      label.textContent = '连接中…';
      break;
    case STATES.CONNECTED:
      ring.classList.add('connected');
      label.textContent = '断开连接';
      break;
    case STATES.ERROR:
      ring.classList.add('error');
      label.textContent = '错误';
      break;
    default:
      label.textContent = '连接';
  }

  // Dots & latency
  if (reachability) {
    setDot('socksDot', reachability.socks_port_open);
    setDot('httpDot', reachability.http_proxy_open);
    setDot('hostDot', reachability.host_reachable);
    if (reachability.latency_ms != null && reachability.host_reachable) {
      setLatency('hostLatency', reachability.latency_ms);
    } else {
      setLatency('hostLatency', null);
    }
  }

  // Error message
  setError(currentState === STATES.ERROR ? errorMessage : null);
}

// ── State transition ──
function setState(state) {
  const prev = currentState;
  if (state === prev) return;
  currentState = state;

  // Uptime lifecycle
  if (state !== STATES.CONNECTED && prev === STATES.CONNECTED) {
    stopUptimeTimer();
    resetStats();
  }

  // Clear data on disconnect
  if (state === STATES.DISCONNECTED) {
    reachability = null;
    errorMessage = null;
  }

  render();
}

// ── Tunnel Connection ──

async function connect() {
  setState(STATES.CONNECTING);
  showTestProgress();
  try {
    await invoke('tunnel_start');
    setState(STATES.CONNECTED);
    hideTestProgress();
    refreshStatus();
  } catch (e) {
    errorMessage = String(e);
    setState(STATES.ERROR);
    hideTestProgress();
  }
}

async function disconnect() {
  try {
    await invoke('tunnel_stop');
    setState(STATES.DISCONNECTED);
  } catch (e) {
    showToast('Disconnect failed: ' + e);
  }
}

function toggleConnection() {
  if (currentState === STATES.CONNECTING) return;
  if (currentState === STATES.CONNECTED) disconnect();
  else connect();
}

async function refreshStatus() {
  try {
    const status = await invoke('tunnel_status');
    reachability = status;

    // Sync state from backend
    const statusMap = {
      'stopped': STATES.DISCONNECTED,
      'connecting': STATES.CONNECTING,
      'connected': STATES.CONNECTED,
      'reconnecting': STATES.CONNECTING,
    };
    const backendState = statusMap[status.status] || STATES.ERROR;
    if (status.status === 'error') {
      errorMessage = status.error_detail;
    }

    // Update stats
    if (status.bytes_up != null) {
      updateStats(status.bytes_up, status.bytes_down);
    }

    // Uptime
    if (status.status === 'connected' && status.uptime_secs != null) {
      startUptimeTimer(status.uptime_secs);
    }

    setState(backendState);
  } catch (e) {
    console.error('Failed to get status:', e);
  }
}

async function testConnectivity() {
  try {
    const report = await invoke('test_connectivity');
    reachability = report;
    render();
  } catch (e) {
    showToast('Test failed: ' + e);
  }
}

// ── Event Listeners ──

function setupEventListeners() {
  listen('tunnel-status', (event) => {
    const statusMap = {
      'stopped': STATES.DISCONNECTED,
      'connecting': STATES.CONNECTING,
      'connected': STATES.CONNECTED,
      'reconnecting': STATES.CONNECTING,
      'error': STATES.ERROR,
    };
    setState(statusMap[event.payload] || STATES.ERROR);
  });

  listen('health-check', (event) => {
    reachability = event.payload;
    if (event.payload.bytes_up != null) {
      updateStats(event.payload.bytes_up, event.payload.bytes_down);
    }
    render();
  });

  listen('tunnel-error', (event) => {
    errorMessage = event.payload;
    setState(STATES.ERROR);
  });

  listen('tunnel-test-progress', (event) => {
    const { step, status } = event.payload;
    updateTestProgress(step, status);
  });

  listen('tray-connect', () => {
    connect();
  });

  listen('tray-disconnect', () => {
    disconnect();
  });

  // Real-time log forwarding from backend
  if (listen) {
    listen('app-log', (event) => {
      appendLogLine(event.payload);
    });
  }
}

// ── Frontend error forwarding to backend logs ──
window.addEventListener('error', (e) => {
  invoke('log_frontend_error', {
    message: `Uncaught: ${e.message} at ${e.filename}:${e.lineno}`,
    level: 'error',
  }).catch(() => {});
});

window.addEventListener('unhandledrejection', (e) => {
  invoke('log_frontend_error', {
    message: `Unhandled rejection: ${e.reason}`,
    level: 'error',
  }).catch(() => {});
});

// ── Profile Management ──

async function loadProfiles() {
  try {
    const [profiles, active, autoStart] = await Promise.all([
      invoke('list_profiles'),
      invoke('get_active_profile'),
      invoke('get_auto_start'),
    ]);
    allProfiles = profiles;
    activeProfileId = active.id;
    populateProfileSelector();
    fillSettingsFromProfile(active);
    updateTunnelDisplay(active);
    document.getElementById('autoStart').checked = autoStart;

    // Fetch local LAN IP
    try {
      const ip = await invoke('get_local_ip');
      const ipEl = document.getElementById('statsIp');
      if (ipEl) ipEl.textContent = ip;
    } catch (_) {}
  } catch (e) {
    console.error('Failed to load profiles:', e);
  }
}

function populateProfileSelector() {
  const nameEl = document.getElementById('profileName');
  const listEl = document.getElementById('profileList');

  const active = allProfiles.find((p) => p.id === activeProfileId);
  nameEl.textContent = active ? active.name || 'Unnamed' : 'Default';

  listEl.innerHTML = '';
  for (const p of allProfiles) {
    const item = document.createElement('div');
    item.className = 'profile-dropdown-item' + (p.id === activeProfileId ? ' active' : '');

    const nameSpan = document.createElement('span');
    nameSpan.className = 'profile-item-name';
    nameSpan.textContent = p.name || 'Unnamed';
    nameSpan.onclick = () => switchProfile(p.id);

    const editBtn = document.createElement('span');
    editBtn.className = 'profile-item-edit';
    editBtn.innerHTML = '<i data-lucide="pencil"></i>';
    editBtn.onclick = (e) => {
      e.stopPropagation();
      editProfile(p.id);
    };

    const deleteBtn = document.createElement('span');
    deleteBtn.className = 'profile-item-delete';
    deleteBtn.innerHTML = '<i data-lucide="trash-2"></i>';
    deleteBtn.onclick = (e) => {
      e.stopPropagation();
      deleteProfileById(p.id);
    };

    item.appendChild(nameSpan);
    item.appendChild(editBtn);
    item.appendChild(deleteBtn);
    listEl.appendChild(item);
  }

  if (window.lucide) window.lucide.createIcons();
}

async function switchProfile(profileId) {
  if (profileId === activeProfileId) {
    closeProfileDropdown();
    return;
  }
  try {
    await invoke('set_active_profile', { profileId });
    activeProfileId = profileId;
    const profile = allProfiles.find((p) => p.id === profileId);
    if (profile) {
      fillSettingsFromProfile(profile);
      updateTunnelDisplay(profile);
    }
    populateProfileSelector();
    closeProfileDropdown();
    refreshStatus();
  } catch (e) {
    showToast('Switch failed: ' + e);
  }
}

async function editProfile(profileId) {
  try {
    isNewProfilePending = false;
    await invoke('set_active_profile', { profileId });
    activeProfileId = profileId;
    const profile = allProfiles.find((p) => p.id === profileId);
    if (profile) {
      fillSettingsFromProfile(profile);
      updateTunnelDisplay(profile);
    }
    populateProfileSelector();
    closeProfileDropdown();
    document.getElementById('settingsTitle').textContent = 'Edit Profile';
    updateSettingsButtons();
    switchTab('server');
    navigate('settings');
  } catch (e) {
    showToast('Edit failed: ' + e);
  }
}

async function saveSettings() {
  const profile = buildProfileFromSettings();
  const autoStart = document.getElementById('autoStart').checked;
  try {
    await invoke('save_profile', { profile });
    await invoke('set_autostart', { enabled: autoStart });
    if (isNewProfilePending) {
      isNewProfilePending = false;
      activeProfileId = profile.id;
      await invoke('set_active_profile', { profileId: profile.id });
    }
    allProfiles = await invoke('list_profiles');
    populateProfileSelector();
    const updated = allProfiles.find((p) => p.id === activeProfileId);
    if (updated) updateTunnelDisplay(updated);
    showToast('Settings saved');
    navigate('home');
  } catch (e) {
    showToast('Save failed: ' + e);
  }
}

function emptyProfile() {
  return {
    id: crypto.randomUUID(),
    name: 'New Profile',
    target_host: '',
    target_user: '',
    password: null,
    ssh_port: 22,
    socks_port: 1080 + allProfiles.length,
    http_proxy_port: 1081 + allProfiles.length,
    ssh_key_path: null,
    auto_proxy: true,
    auto_reconnect: true,
    test_url: null,
  };
}

function newProfile() {
  isNewProfilePending = true;
  const empty = emptyProfile();
  fillSettingsFromProfile(empty);
  closeProfileDropdown();
  document.getElementById('settingsTitle').textContent = 'New Profile';
  updateSettingsButtons();
  switchTab('server');
  navigate('settings');
  document.getElementById('profileNameInput').focus();
  document.getElementById('profileNameInput').select();
}

async function deleteProfile() {
  if (!activeProfileId) return;
  await deleteProfileById(activeProfileId);
}

async function deleteProfileById(id) {
  if (allProfiles.length <= 1) {
    showToast('Cannot delete last profile');
    return;
  }
  try {
    const confirmed = await invoke('confirm_dialog', { message: 'Delete this profile?' });
    if (!confirmed) return;
    await invoke('delete_profile', { profileId: id });
    allProfiles = await invoke('list_profiles');
    const active = await invoke('get_active_profile');
    activeProfileId = active.id;
    populateProfileSelector();
    fillSettingsFromProfile(active);
    updateTunnelDisplay(active);
    showToast('Profile deleted');
    const settingsPage = document.getElementById('page-settings');
    if (settingsPage && settingsPage.classList.contains('active')) {
      navigate('home');
    }
  } catch (e) {
    showToast('Delete failed: ' + e);
  }
}

function buildProfileFromSettings() {
  const existing = allProfiles.find((p) => p.id === activeProfileId);
  const passwordVal = document.getElementById('password').value;
  return {
    id: isNewProfilePending ? crypto.randomUUID() : activeProfileId,
    name: document.getElementById('profileNameInput').value || 'Unnamed',
    target_host: document.getElementById('targetHost').value,
    target_user: document.getElementById('targetUser').value,
    password: passwordVal || null,
    ssh_port: parseInt(document.getElementById('sshPort').value) || 22,
    socks_port: parseInt(document.getElementById('socksPort').value) || 1080,
    http_proxy_port: parseInt(document.getElementById('httpPort').value) || 1081,
    ssh_key_path: existing?.ssh_key_path || null,
    auto_proxy: document.getElementById('autoProxy').checked,
    auto_reconnect: document.getElementById('autoReconnect').checked,
    test_url: document.getElementById('testUrl').value || null,
  };
}

function fillSettingsFromProfile(profile) {
  document.getElementById('profileNameInput').value = profile.name || '';
  document.getElementById('targetHost').value = profile.target_host || '';
  document.getElementById('targetUser').value = profile.target_user || '';
  document.getElementById('password').value = profile.password || '';
  document.getElementById('sshPort').value = profile.ssh_port || 22;
  document.getElementById('socksPort').value = profile.socks_port || 1080;
  document.getElementById('httpPort').value = profile.http_proxy_port || 1081;
  document.getElementById('autoProxy').checked = profile.auto_proxy !== false;
  document.getElementById('autoReconnect').checked = profile.auto_reconnect !== false;
  document.getElementById('testUrl').value = profile.test_url || '';
}

function updateTunnelDisplay(profile) {
  const cardTitle = document.getElementById('tunnelProfileName');
  if (cardTitle) cardTitle.textContent = profile.name || 'Tunnel';

  const targetEl = document.getElementById('targetInfo');
  if (profile.target_user && profile.target_host) {
    targetEl.textContent = `${profile.target_user}@${profile.target_host}:${profile.ssh_port || 22}`;
  } else if (profile.target_host) {
    targetEl.textContent = `${profile.target_host}:${profile.ssh_port || 22}`;
  } else {
    targetEl.textContent = '—';
  }

  document.getElementById('socksPortDisplay').textContent = profile.socks_port || 1080;
  document.getElementById('httpPortDisplay').textContent = profile.http_proxy_port || 1081;

  setLatency('hostLatency', null);
  setUptime(null);
  setError(null);
  setDot('socksDot', false);
  setDot('httpDot', false);
  setDot('hostDot', false);
}

function updateSettingsButtons() {
  const delBtn = document.querySelector('.btn-delete');
  if (delBtn) {
    delBtn.style.display = isNewProfilePending ? 'none' : '';
  }
}

// ── Profile Dropdown ──

function toggleProfileDropdown() {
  const dd = document.getElementById('profileDropdown');
  dd.classList.toggle('open');
}

function closeProfileDropdown() {
  const dd = document.getElementById('profileDropdown');
  dd.classList.remove('open');
}

document.addEventListener('click', (e) => {
  const selector = document.getElementById('profileSelector');
  if (selector && !selector.contains(e.target)) {
    closeProfileDropdown();
  }
});

// ── Page Navigation ──

function navigate(page) {
  document.querySelectorAll('.page').forEach(p => p.classList.remove('active'));
  const target = document.getElementById('page-' + page);
  if (target) target.classList.add('active');
  if (page === 'logs') loadLogs();
}

// ── Config Tabs ──

function switchTab(tabName) {
  document.querySelectorAll('.config-tab').forEach(t => t.classList.remove('active'));
  document.querySelectorAll('.config-tab-panel').forEach(p => p.classList.remove('active'));
  const tab = document.querySelector(`.config-tab[data-tab="${tabName}"]`);
  const panel = document.getElementById(`tab-${tabName}`);
  if (tab) tab.classList.add('active');
  if (panel) panel.classList.add('active');
}

// ── UI Helpers ──

let uptimeInterval = null;
let uptimeBaseSecs = null;

function setDot(id, ok) {
  const dot = document.getElementById(id);
  if (dot) dot.className = 'data-dot' + (ok ? ' active' : '');
}

function setLatency(id, ms) {
  const el = document.getElementById(id);
  if (!el) return;
  if (ms != null) {
    el.textContent = ms + 'ms';
    el.classList.add('active');
  } else {
    el.textContent = '';
    el.classList.remove('active');
  }
}

function setUptime(secs) {
  const el = document.getElementById('tunnelUptime');
  if (!el) return;
  if (secs != null) {
    el.textContent = formatUptime(secs);
    el.classList.add('active');
  } else {
    el.textContent = '';
    el.classList.remove('active');
  }
}

function setError(msg) {
  const el = document.getElementById('tunnelError');
  if (!el) return;
  el.textContent = msg || '';
}

function formatUptime(totalSecs) {
  const h = Math.floor(totalSecs / 3600);
  const m = Math.floor((totalSecs % 3600) / 60);
  const s = totalSecs % 60;
  return String(h).padStart(2, '0') + ':' + String(m).padStart(2, '0') + ':' + String(s).padStart(2, '0');
}

function startUptimeTimer(baseSecs) {
  stopUptimeTimer();
  uptimeBaseSecs = baseSecs;
  setUptime(uptimeBaseSecs);
  uptimeInterval = setInterval(() => {
    uptimeBaseSecs++;
    setUptime(uptimeBaseSecs);
  }, 1000);
}

function stopUptimeTimer() {
  if (uptimeInterval) {
    clearInterval(uptimeInterval);
    uptimeInterval = null;
  }
  uptimeBaseSecs = null;
  setUptime(null);
}

// ── Traffic Stats ──

let prevBytesUp = null;
let prevBytesDown = null;
let prevStatsTime = null;

function formatBytes(bytes) {
  if (bytes == null || bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const val = bytes / Math.pow(1024, i);
  return (i === 0 ? val.toFixed(0) : val.toFixed(1)) + ' ' + units[i];
}

function formatSpeed(bytesPerSec) {
  if (bytesPerSec == null || bytesPerSec < 1) return '0 B/s';
  return formatBytes(Math.round(bytesPerSec)) + '/s';
}

function updateStats(bytesUp, bytesDown) {
  const totalUpEl = document.getElementById('statsTotalUp');
  const totalDownEl = document.getElementById('statsTotalDown');
  if (totalUpEl) totalUpEl.textContent = formatBytes(bytesUp);
  if (totalDownEl) totalDownEl.textContent = formatBytes(bytesDown);

  const now = Date.now();
  if (prevBytesUp !== null && prevStatsTime !== null) {
    const dt = (now - prevStatsTime) / 1000;
    if (dt > 0) {
      const upSpeed = (bytesUp - prevBytesUp) / dt;
      const downSpeed = (bytesDown - prevBytesDown) / dt;
      const upSpeedEl = document.getElementById('statsUpSpeed');
      const downSpeedEl = document.getElementById('statsDownSpeed');
      if (upSpeedEl) upSpeedEl.textContent = formatSpeed(upSpeed);
      if (downSpeedEl) downSpeedEl.textContent = formatSpeed(downSpeed);
    }
  }
  prevBytesUp = bytesUp;
  prevBytesDown = bytesDown;
  prevStatsTime = now;
}

function resetStats() {
  prevBytesUp = null;
  prevBytesDown = null;
  prevStatsTime = null;
  const totalUpEl = document.getElementById('statsTotalUp');
  const totalDownEl = document.getElementById('statsTotalDown');
  const upSpeedEl = document.getElementById('statsUpSpeed');
  const downSpeedEl = document.getElementById('statsDownSpeed');
  if (totalUpEl) totalUpEl.textContent = '0 B';
  if (totalDownEl) totalDownEl.textContent = '0 B';
  if (upSpeedEl) upSpeedEl.textContent = '—';
  if (downSpeedEl) downSpeedEl.textContent = '—';
}

// ── Toast ──

function showToast(msg) {
  const toast = document.getElementById('toast');
  if (toast) {
    toast.textContent = msg;
    toast.style.display = 'block';
    setTimeout(() => { toast.style.display = 'none'; }, 2500);
  }
}

// ── Test Progress ──

function showTestProgress() {
  const el = document.getElementById('testProgress');
  if (!el) return;
  el.style.display = 'flex';
  ['testSocks', 'testHttp', 'testTarget'].forEach(id => {
    const item = document.getElementById(id);
    if (item) item.className = 'test-progress-item';
  });
}

function hideTestProgress() {
  const el = document.getElementById('testProgress');
  if (el) el.style.display = 'none';
}

function updateTestProgress(step, status) {
  const idMap = { socks: 'testSocks', http: 'testHttp', target: 'testTarget' };
  const id = idMap[step];
  if (!id) return;
  const item = document.getElementById(id);
  if (!item) return;
  item.className = 'test-progress-item ' + status;
}

// ── Log Viewer ──

async function loadLogs() {
  try {
    const lines = await invoke('get_recent_logs', { lines: 200 });
    const container = document.getElementById('logEntries');
    container.innerHTML = '';
    for (const line of lines) {
      appendLogLine(line);
    }
    container.scrollTop = container.scrollHeight;
  } catch (e) {
    console.error('Failed to load logs:', e);
  }
}

function appendLogLine(line) {
  const container = document.getElementById('logEntries');
  if (!container) return;

  const el = document.createElement('div');
  el.className = 'log-line';

  if (line.includes(' ERROR ') || line.includes('[error]')) {
    el.classList.add('log-error');
  } else if (line.includes(' WARN ') || line.includes('[warn]')) {
    el.classList.add('log-warn');
  } else if (line.includes(' INFO ')) {
    el.classList.add('log-info');
  } else {
    el.classList.add('log-debug');
  }

  el.textContent = line;
  container.appendChild(el);

  while (container.children.length > 500) {
    container.removeChild(container.firstChild);
  }

  container.scrollTop = container.scrollHeight;
}

function clearLogs() {
  const container = document.getElementById('logEntries');
  if (container) container.innerHTML = '';
}

// ── Init ──

window.addEventListener('DOMContentLoaded', () => {
  const appEl = document.querySelector('.app');
  console.log('[PassPort] DOMContentLoaded — .app children:', appEl?.children.length);
  console.log('[PassPort] body computed bg:', getComputedStyle(document.body).backgroundColor);
  console.log('[PassPort] html computed bg:', getComputedStyle(document.documentElement).backgroundColor);

  loadProfiles();
  setupEventListeners();

  if (window.lucide) window.lucide.createIcons();
});

// Expose functions for onclick handlers
window.connect = connect;
window.disconnect = disconnect;
window.toggleConnection = toggleConnection;
window.navigate = navigate;
window.refreshStatus = refreshStatus;
window.testConnectivity = testConnectivity;
window.saveSettings = saveSettings;
window.newProfile = newProfile;
window.editProfile = editProfile;
window.deleteProfile = deleteProfile;
window.switchTab = switchTab;
window.toggleProfileDropdown = toggleProfileDropdown;
window.loadLogs = loadLogs;
window.clearLogs = clearLogs;