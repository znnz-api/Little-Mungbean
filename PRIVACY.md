# Privacy / 隐私说明

## English

Little-Mungbean contains no telemetry, advertising, or behavioral analytics.

Configuration, logs, client configuration backups, and usage statistics are stored locally under `%LOCALAPPDATA%\Little-Mungbean`. On macOS, application data is in `~/Library/Application Support/Little-Mungbean` and icon caches are in `~/Library/Caches/Little-Mungbean`. Saved API Keys use Windows DPAPI on Windows, or authenticated encryption backed by macOS Keychain on macOS.

In gateway mode, API Keys and model requests are sent to the provider selected by the user. Client installation accesses the relevant download sources or npm registries. The application also fetches the cloud provider preset list and icons; these requests do not upload user configuration or API Keys.

Usage statistics and request metadata are stored in the local SQLite database, `usage.sqlite3`, and are not uploaded to an analytics service. The database does not store plaintext API Keys, prompts, or model replies. Little-Mungbean does not estimate billing.

## 中文

Little-Mungbean 不包含遥测、广告或行为分析功能。

配置、日志、客户端配置备份和用量统计保存在本机 `%LOCALAPPDATA%\Little-Mungbean` 目录。macOS 应用数据位于 `~/Library/Application Support/Little-Mungbean`，图标缓存位于 `~/Library/Caches/Little-Mungbean`。保存的 API Key 在 Windows 使用 DPAPI 加密，在 macOS 使用由 Keychain 保存密钥的认证加密。

网关模式下，API Key 和模型请求会发送给用户选择的提供商。安装客户端时访问相应下载源或 npm 源。程序还会获取云端提供商预设清单与图标，此过程不会上传用户配置或 API Key。

用量统计与请求元数据保存在本地 SQLite 数据库 `usage.sqlite3`，不上传到统计服务。数据库不保存明文 API Key、请求正文或模型回答，也不估算费用。
