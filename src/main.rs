use std::path::Path;
use std::fs;
use winreg::enums::*;
use winreg::RegKey;
use regex::Regex;
use reqwest;
use serde_json::Value;
use std::time::Duration;
use std::io::{self, Write};
use tokio::time::sleep;
use futures::stream::{self, StreamExt};
use std::sync::Arc;

// 应用信息结构体
#[derive(Debug, Clone)]
struct AppInfo {
    app_id: u64,
    icon_id: String,
    name: String,
}

// 等待用户按 Enter 键退出
fn wait_and_exit(message: &str, exit_code: i32) -> ! {
    println!("{}", message);
    print!("按 Enter 键退出...");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    std::process::exit(exit_code);
}

// 读取注册表值
fn read_regedit(root: RegKey, path: &str, name: &str) -> Result<String, String> {
    let subkey = root.open_subkey(path)
        .map_err(|e| format!("打开注册表失败: {}", e))?;
    subkey.get_value(name)
        .map_err(|e| format!("读取值失败: {}", e))
}

// 解析 VDF 文件
fn parse_vdf(steam_path: &str) -> Result<Vec<u64>, String> {
    let file_path = format!("{}\\steamapps\\libraryfolders.vdf", steam_path);
    if !Path::new(&file_path).exists() {
        return Err(format!("文件不存在: {}", file_path));
    }
    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("读取VDF文件失败：{}", e))?;

    let re = Regex::new(r#""(\d+)"\s+"\d+""#)
        .map_err(|e| format!("正则表达式编译失败: {}", e))?;

    let mut app_ids = Vec::new();
    for cap in re.captures_iter(&content) {
        if let Ok(id) = cap[1].parse::<u64>() {
            app_ids.push(id);
        }
    }
    Ok(app_ids)
}

// 异步获取单个应用信息
async fn fetch_app_info(app_id: u64, client: &reqwest::Client) -> Result<Option<AppInfo>, String> {
    let url = format!("http://steama.ddxnb.cn/v1/info/{}", app_id);

    // 添加延迟避免请求过快
    sleep(Duration::from_millis(500)).await;

    let response = client.get(&url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("请求失败 (appid={}): {}", app_id, e))?;

    if !response.status().is_success() {
        return Err(format!("请求失败 (appid={}): {}", app_id, response.status()));
    }

    let result: Value = response.json()
        .await
        .map_err(|e| format!("解析 JSON 失败 (appid={}): {}", app_id, e))?;

    let data = match result.get("data").and_then(|d| d.get(&app_id.to_string())) {
        Some(d) => d,
        None => return Err(format!("未找到 app_id: {}", app_id)),
    };

    let common = match data.get("common") {
        Some(c) => c,
        None => return Err(format!("未找到 common: {}", app_id)),
    };

    let client_icon = match common.get("clienticon").and_then(|i| i.as_str()) {
        Some(icon) => icon.to_string(),
        None => return Err(format!("未找到 clienticon: {}", app_id)),
    };

    let app_name = match common.get("name").and_then(|n| n.as_str()) {
        Some(name) => name.to_string(),
        None => return Err(format!("未找到 name: {}", app_id)),
    };

    Ok(Some(AppInfo {
        app_id,
        icon_id: client_icon,
        name: app_name,
    }))
}

// 异步下载单个图标
async fn download_app_icon(app_info: &AppInfo, steam_path: &str, client: &reqwest::Client) -> Result<(), String> {
    let url = format!(
        "http://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/{}/{}.ico",
        app_info.app_id, app_info.icon_id
    );

    let icon_path = format!("{}/steam/games/{}.ico", steam_path, app_info.icon_id);

    // 确保目录存在
    if let Some(parent) = Path::new(&icon_path).parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录失败: {}", e))?;
    }

    // 检查文件是否已存在
    if Path::new(&icon_path).exists() {
        println!("✅️ 图标已存在: {} ({})", app_info.name, app_info.app_id);
        return Ok(());
    }

    // 下载图标
    let response = client.get(&url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("下载失败 ({}): {}", app_info.name, e))?;

    if !response.status().is_success() {
        return Err(format!("下载失败 ({}): HTTP {}", app_info.name, response.status()));
    }

    // 读取图标数据
    let icon_data = response.bytes()
        .await
        .map_err(|e| format!("读取数据失败 ({}): {}", app_info.name, e))?;

    // 写入图标文件
    fs::write(&icon_path, icon_data)
        .map_err(|e| format!("写入文件失败 ({}): {}", app_info.name, e))?;

    println!("📦️ 图标已下载: {} ({})", app_info.name, app_info.app_id);
    Ok(())
}

// 异步处理所有应用
async fn process_apps(app_ids: Vec<u64>, steam_path: String) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .build()
        .expect("创建HTTP客户端失败");

    let client = Arc::new(client);
    let steam_path = Arc::new(steam_path);

    // 控制并发数量，避免过多请求
    let concurrency_limit = 5;

    // 使用流处理每个应用
    let results = stream::iter(app_ids)
        .map(|app_id| {
            let client = client.clone();
            let steam_path = steam_path.clone();

            async move {
                // 获取应用信息
                match fetch_app_info(app_id, &client).await {
                    Ok(Some(app_info)) => {
                        println!("📝 获取应用信息: {} ({})", app_info.name, app_info.app_id);

                        // 立即下载图标
                        if let Err(e) = download_app_icon(&app_info, &steam_path, &client).await {
                            eprintln!("⚡️ 下载失败: {}", e);
                        }

                        Some(app_info)
                    }
                    Ok(None) => {
                        eprintln!("⚠️ 未找到应用信息: {}", app_id);
                        None
                    }
                    Err(e) => {
                        eprintln!("⚠️ {}", e);
                        None
                    }
                }
            }
        })
        .buffer_unordered(concurrency_limit)
        .collect::<Vec<_>>()
        .await;

    // 统计结果
    let success_count = results.iter().filter(|r| r.is_some()).count();
    println!("\n🎯 完成! 成功处理 {} 个应用", success_count);
}

#[tokio::main]
async fn main() {
    // 读取 Steam 安装路径
    let steam_path = read_regedit(
        RegKey::predef(HKEY_LOCAL_MACHINE),
        r"SOFTWARE\Wow6432Node\Valve\Steam",
        "InstallPath"
    ).unwrap_or_else(|err| wait_and_exit(&err, 1));

    println!("📂 Steam 路径: {}", steam_path);

    // 获取 Steam 应用列表
    let app_list = parse_vdf(&steam_path).unwrap_or_else(|err| wait_and_exit(&err, 1));
    println!("📋 找到 {} 个应用", app_list.len());

    // 异步处理所有应用
    process_apps(app_list, steam_path).await;

    wait_and_exit("所有任务完成!", 0);
}
