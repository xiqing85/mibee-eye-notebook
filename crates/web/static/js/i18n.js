// Bilingual dictionary (zh/en). t(key) resolves against the current language,
// falling back to the raw key so untranslated entries stay visible.

import { store } from './store.js';
import { $ } from './ui.js';

const DICT = {
  zh: {
    appTitle: 'MiBee 摄像头',
    loginSubtitle: '统一设备管理',
    username: '用户名', password: '密码', passwordConfirm: '确认密码',
    loginBtn: '登录', setupBtn: '设置管理员并进入',
    newPassword: '设置管理密码', newUsername: '管理员用户名',
    loginInvalid: '用户名或密码错误',
    rateLimited: '尝试过于频繁，请稍后再试',
    passwordMismatch: '两次输入的密码不一致',
    passwordTooShort: '密码至少 8 个字符',
    setupDone: '管理员已创建',
    logout: '退出登录',
    live: '实时', cameras: '相机', settings: '设置', status: '状态', devices: '设备',
    loading: '加载中...', retry: '重试', save: '保存', saving: '保存中...',
    saved: '已保存', saveError: '保存失败', fetchError: '网络错误',
    error: '错误', success: '成功',
    cancel: '取消', confirm: '确认',
    setupHint: '首次使用，设置管理员账号完成初始化',
    showPassword: '显示密码', hidePassword: '隐藏密码',
    switchLang: '切换语言', themeLabel: '切换主题',
    // Live view
    streamError: '视频流不可用', streamTimeout: '视频流无响应',
    reconnecting: '重连中...', fallbackMjpeg: 'MJPEG 模式',
    snapshot: '截图', fullscreen: '全屏', snapshotFail: '截图失败',
    videoAria: '实时监控画面', liveCamera: '实时画面',
    hflip: '水平翻转', vflip: '垂直翻转',
    noCamera: '没有可用的相机',
    startStream: '启动', stopStream: '停止',
    streamStarted: '采集已启动', streamStopped: '采集已停止',
    recordingStart: '开始录像', recordingStop: '停止录像',
    recordingStarted: '录像已开始', recordingStopped: '录像已停止',
    // PTZ
    ptzControl: '云台控制', ptzEnableDesc: '在实时画面显示方向控制按钮',
    pan: '水平', tilt: '俯仰', zoom: '变焦',
    ptzPosition: '云台位置',
    // Imaging
    imagingTitle: '成像控制', imagingLoad: '成像参数加载失败',
    imagingReset: '恢复默认', imagingResetConfirm: '将所有成像参数恢复默认值？',
    paramError: '参数 {name} 设置失败',
    'imaging.Brightness': '亮度', 'imaging.Contrast': '对比度',
    'imaging.Saturation': '饱和度', 'imaging.Sharpness': '锐度',
    'imaging.AWBMode': '白平衡', 'imaging.ExposureMode': '曝光模式',
    'imaging.HFlip': '水平翻转(传感器)', 'imaging.VFlip': '垂直翻转(传感器)',
    // Cameras view
    camerasTitle: '相机', camerasDesc: '已接入的相机与采集状态', addCamera: '添加相机', cameraName: '名称',
    cameraModel: '型号', cameraFirmware: '固件', resolution: '分辨率', fps: '帧率',
    statusOnline: '在线', statusOffline: '离线', statusRunning: '采集中', statusIdle: '空闲',
    statusLabel: '状态',
    deleteCamera: '删除', deleteConfirm: '删除相机 {name}？',
    openLive: '查看实时', noCameras: '尚未添加相机',
    // Settings view
    configTitle: '系统配置', unsaved: '有未保存的更改',
    unsavedConfirm: '放弃未保存的更改？',
    validationError: '请检查输入', loadError: '加载配置失败',
    configChanged: '配置已被其他客户端修改',
    applyRestart: '部分设置保存后需重启设备生效',
    applyRestartSec: '需重启生效', applyImmediateSec: '立即生效',
    restartNow: '立即重启', restartConfirm: '现在重启设备服务？直播与页面会中断数秒。',
    restarting: '设备重启中…', restartTimeout: '重启超时，请稍后手动刷新页面。',
    savedNeedRestart: '已保存，重启后生效：{sections}',
    // Status view
    statusTitle: '系统状态', statusDesc: '设备运行状态一览', connection: '连接', cameraInfo: '相机信息',
    events: '事件通道', apiLabel: 'API',
    connected: '已连接', disconnected: '已断开', checking: '检查中...',
    uptime: '运行时长', deviceName: '设备名称', model: '型号',
    firmware: '固件版本', vendor: '厂商',
    recordingService: '录像服务', gb28181Service: 'GB28181',
    stateOn: '运行中', stateOff: '已停止',
    // Devices view
    devicesTitle: '主机设备', devicesDesc: '主机上的视频 / 音频采集设备', videoDevices: '视频设备', audioDevices: '音频设备',
    formats: '支持格式', useAsCamera: '用作相机', noDevices: '未发现设备',
    hostCaps: '主机能力', cores: 'CPU 核心', memory: '内存',
    encoder: '推荐编码器',
    // Config field labels (shared sections)
    web: 'Web 服务', camera: '图像采集', rtsp: 'RTSP', onvif: 'ONVIF',
    logging: '日志',
    'web.enabled': '启用 Web', 'web.port': 'Web 端口',
    'web.username': '用户名', 'web.password': '密码',
    'camera.mode': '采集模式', 'camera.width': '宽度', 'camera.height': '高度',
    'camera.fps': '帧率', 'camera.bitrate': '码率', 'camera.codec': '编码',
    'camera.rotation': '旋转（仅网页显示）', 'camera.device': '设备路径',
    'camera.hflip': '永久水平翻转（烘焙进视频流）', 'camera.vflip': '永久上下翻转（烘焙进视频流）',
    'camera.hflip': '水平翻转（设备级）', 'camera.vflip': '垂直翻转（设备级·倒装）',
    flipH: '设备级水平翻转', flipV: '设备级垂直翻转', flipApplied: '翻转设置已应用',
    'rtsp.port': 'RTSP 端口', 'rtsp.username': 'RTSP 用户名', 'rtsp.password': 'RTSP 密码',
    'onvif.port': 'ONVIF 端口', 'onvif.username': 'ONVIF 用户名', 'onvif.password': 'ONVIF 密码',
    'logging.level': '日志级别',
    'device.name': '设备名称', 'device.manufacturer': '厂商', 'device.model': '型号',
    'device.firmware': '固件版本', 'device.serial_number': '序列号',
    'recording.enabled': '启用录像', 'recording.path': '存储路径',
    'recording.segment_secs': '分片时长（秒）', 'recording.retention_days': '保留天数',
    gb28181: 'GB28181 设置',
    'gb28181.enabled': '启用',
    'gb28181.platform_sip_address': '平台 SIP 地址',
    'gb28181.platform_sip_port': '平台 SIP 端口',
    'gb28181.device_id': '设备 ID',
    'gb28181.channel_id': '通道 ID',
    'gb28181.sip_domain': 'SIP 域',
    'gb28181.password': 'SIP 密码',
    'gb28181.local_sip_port': '本地 SIP 端口',
    'gb28181.register_interval_secs': '注册间隔（秒）',
    'gb28181.heartbeat_interval_secs': '心跳间隔（秒）',
    'gb28181.heartbeat_timeout_count': '心跳超时次数',
    'gb28181.id20Placeholder': '20 位数字',
  },
  en: {
    appTitle: 'MiBee Cam',
    loginSubtitle: 'Unified device management',
    username: 'Username', password: 'Password', passwordConfirm: 'Confirm password',
    loginBtn: 'Sign In', setupBtn: 'Create admin & sign in',
    newPassword: 'Set admin password', newUsername: 'Admin username',
    loginInvalid: 'Wrong username or password',
    rateLimited: 'Too many attempts, try again later',
    passwordMismatch: 'Passwords do not match',
    passwordTooShort: 'Password must be at least 8 characters',
    setupDone: 'Admin created',
    logout: 'Logout',
    live: 'Live', cameras: 'Cameras', settings: 'Settings', status: 'Status', devices: 'Devices',
    loading: 'Loading...', retry: 'Retry', save: 'Save', saving: 'Saving...',
    saved: 'Saved', saveError: 'Failed to save', fetchError: 'Network error',
    error: 'Error', success: 'Success',
    cancel: 'Cancel', confirm: 'Confirm',
    setupHint: 'First run — create the admin account to finish setup',
    showPassword: 'Show password', hidePassword: 'Hide password',
    switchLang: 'Switch language', themeLabel: 'Toggle theme',
    // Live view
    streamError: 'Stream unavailable', streamTimeout: 'Stream not responding',
    reconnecting: 'Reconnecting...', fallbackMjpeg: 'MJPEG mode',
    snapshot: 'Snapshot', fullscreen: 'Fullscreen', snapshotFail: 'Snapshot failed',
    videoAria: 'Live camera feed', liveCamera: 'Live',
    hflip: 'Horizontal flip', vflip: 'Vertical flip',
    noCamera: 'No camera available',
    startStream: 'Start', stopStream: 'Stop',
    streamStarted: 'Capture started', streamStopped: 'Capture stopped',
    recordingStart: 'Start recording', recordingStop: 'Stop recording',
    recordingStarted: 'Recording started', recordingStopped: 'Recording stopped',
    // PTZ
    ptzControl: 'PTZ Control', ptzEnableDesc: 'Show direction controls on the Live view',
    pan: 'Pan', tilt: 'Tilt', zoom: 'Zoom',
    ptzPosition: 'PTZ Position',
    // Imaging
    imagingTitle: 'Imaging Controls', imagingLoad: 'Failed to load imaging params',
    imagingReset: 'Reset defaults', imagingResetConfirm: 'Reset all imaging parameters to defaults?',
    paramError: 'Failed to set {name}',
    'imaging.Brightness': 'Brightness', 'imaging.Contrast': 'Contrast',
    'imaging.Saturation': 'Saturation', 'imaging.Sharpness': 'Sharpness',
    'imaging.AWBMode': 'White balance', 'imaging.ExposureMode': 'Exposure mode',
    'imaging.HFlip': 'H-flip (sensor)', 'imaging.VFlip': 'V-flip (sensor)',
    // Cameras view
    camerasTitle: 'Cameras', camerasDesc: 'Connected cameras and capture state', addCamera: 'Add camera', cameraName: 'Name',
    cameraModel: 'Model', cameraFirmware: 'Firmware', resolution: 'Resolution', fps: 'FPS',
    statusOnline: 'Online', statusOffline: 'Offline', statusRunning: 'Running', statusIdle: 'Idle',
    statusLabel: 'Status',
    deleteCamera: 'Delete', deleteConfirm: 'Delete camera {name}?',
    openLive: 'Open live', noCameras: 'No cameras yet',
    // Settings view
    configTitle: 'Configuration', unsaved: 'Unsaved changes',
    unsavedConfirm: 'Discard unsaved changes?',
    validationError: 'Please check your input', loadError: 'Failed to load config',
    configChanged: 'Settings changed by another client',
    applyRestart: 'Some settings need a device restart to take effect',
    applyRestartSec: 'Restart to apply', applyImmediateSec: 'Immediate',
    restartNow: 'Restart now', restartConfirm: 'Restart the device service now? Streams and the page drop for a few seconds.',
    restarting: 'Restarting…', restartTimeout: 'Restart timed out — refresh the page manually.',
    savedNeedRestart: 'Saved — restart to apply: {sections}',
    // Status view
    statusTitle: 'System Status', statusDesc: 'Device health at a glance', connection: 'Connection', cameraInfo: 'Camera Info',
    events: 'Events', apiLabel: 'API',
    connected: 'Connected', disconnected: 'Disconnected', checking: 'Checking...',
    uptime: 'Uptime', deviceName: 'Device name', model: 'Model',
    firmware: 'Firmware', vendor: 'Vendor',
    recordingService: 'Recording', gb28181Service: 'GB28181',
    stateOn: 'On', stateOff: 'Off',
    // Devices view
    devicesTitle: 'Host Devices', devicesDesc: 'Video / audio capture devices on this host', videoDevices: 'Video devices', audioDevices: 'Audio devices',
    formats: 'Formats', useAsCamera: 'Use as camera', noDevices: 'No devices found',
    hostCaps: 'Host capabilities', cores: 'CPU cores', memory: 'Memory',
    encoder: 'Recommended encoder',
    // Config field labels (shared sections)
    web: 'Web service', camera: 'Capture', rtsp: 'RTSP', onvif: 'ONVIF',
    logging: 'Logging',
    'web.enabled': 'Enable web', 'web.port': 'Web port',
    'web.username': 'Username', 'web.password': 'Password',
    'camera.mode': 'Capture mode', 'camera.width': 'Width', 'camera.height': 'Height',
    'camera.fps': 'FPS', 'camera.bitrate': 'Bitrate', 'camera.codec': 'Codec',
    'camera.rotation': 'Rotation (web display only)', 'camera.device': 'Device path',
    'camera.hflip': 'Permanent H-flip (baked into stream)', 'camera.vflip': 'Permanent V-flip (baked into stream)',
    'camera.hflip': 'Horizontal flip (device)', 'camera.vflip': 'Vertical flip (device, inverted mount)',
    flipH: 'Device-level horizontal flip', flipV: 'Device-level vertical flip', flipApplied: 'Flip applied',
    'rtsp.port': 'RTSP port', 'rtsp.username': 'RTSP username', 'rtsp.password': 'RTSP password',
    'onvif.port': 'ONVIF port', 'onvif.username': 'ONVIF username', 'onvif.password': 'ONVIF password',
    'logging.level': 'Log level',
    'device.name': 'Device name', 'device.manufacturer': 'Manufacturer', 'device.model': 'Model',
    'device.firmware': 'Firmware', 'device.serial_number': 'Serial number',
    'recording.enabled': 'Enable recording', 'recording.path': 'Storage path',
    'recording.segment_secs': 'Segment secs', 'recording.retention_days': 'Retention days',
    gb28181: 'GB28181 Settings',
    'gb28181.enabled': 'Enabled',
    'gb28181.platform_sip_address': 'Platform SIP address',
    'gb28181.platform_sip_port': 'Platform SIP port',
    'gb28181.device_id': 'Device ID',
    'gb28181.channel_id': 'Channel ID',
    'gb28181.sip_domain': 'SIP domain',
    'gb28181.password': 'SIP password',
    'gb28181.local_sip_port': 'Local SIP port',
    'gb28181.register_interval_secs': 'Register interval (s)',
    'gb28181.heartbeat_interval_secs': 'Heartbeat interval (s)',
    'gb28181.heartbeat_timeout_count': 'Heartbeat timeout count',
    'gb28181.id20Placeholder': '20 digits',
  },
};

export function t(key, vars) {
  const table = DICT[store.lang] || DICT.en;
  let s = table[key] !== undefined ? table[key] : (DICT.en[key] !== undefined ? DICT.en[key] : key);
  if (vars) {
    for (const [k, v] of Object.entries(vars)) s = s.replace('{' + k + '}', String(v));
  }
  return s;
}

/// Config field path label: prefer a translation, fall back to the raw key.
export function cfgLabel(path, key) {
  const v = t(path);
  return v === path ? key : v;
}

export function applyLang() {
  document.documentElement.lang = store.lang;
  document.title = t('appTitle');
  document.querySelectorAll('[data-i18n]').forEach((node) => {
    node.textContent = t(node.dataset.i18n);
  });
  document.querySelectorAll('.lang-btn').forEach((b) => {
    b.textContent = store.lang === 'zh' ? 'EN' : '中';
    b.setAttribute('aria-pressed', String(store.lang === 'zh'));
  });
  const v = $('stream-video');
  if (v) v.setAttribute('aria-label', t('videoAria'));
}
