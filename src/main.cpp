/**
 * @file main.cpp
 * @brief wash 主程序入口
 * 
 * 实现命令行参数解析、交互循环、readline 集成和 rc 文件加载。
 * 
 * @author wash
 * @date 2026-09-12
 */

#include <iostream>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>
#include <algorithm>
#include <cstdlib>
#include <readline/readline.h>
#include <readline/history.h>

#include "lexer.h"
#include "parser.h"
#include "executor.h"

/**
 * @brief 默认 PS1 提示符
 */
static const char* DEFAULT_PS1 = "[\\u@\\h \\W]\\$ ";

/**
 * @brief 默认历史记录最大条数
 */
static const int DEFAULT_HISTORY_MAX = 10000;

/**
 * @brief 获取用户主目录
 * @return 主目录路径
 */
std::string getHomeDir() {
#ifdef _WIN32
    const char* home = getenv("USERPROFILE");
    if (!home) {
        home = getenv("HOMEDRIVE");
        if (home) {
            std::string path = std::string(home) + getenv("HOMEPATH");
            return path;
        }
        return ".";
    }
    return home;
#else
    const char* home = getenv("HOME");
    return home ? home : ".";
#endif
}

/**
 * @brief 获取用户名
 * @return 用户名
 */
std::string getUsername() {
#ifdef _WIN32
    const char* user = getenv("USERNAME");
#else
    const char* user = getenv("USER");
#endif
    return user ? user : "unknown";
}

/**
 * @brief 获取主机名
 * @return 主机名
 */
std::string getHostname() {
    char hostname[256];
#ifdef _WIN32
    DWORD size = sizeof(hostname);
    GetComputerNameA(hostname, &size);
#else
    gethostname(hostname, sizeof(hostname));
#endif
    return hostname;
}

/**
 * @brief 获取当前工作目录（简化版，只显示最后一级）
 * @return 目录名
 */
std::string getCurrentDir() {
    char* cwd = getcwd(nullptr, 0);
    if (!cwd) {
        return ".";
    }
    std::string path(cwd);
    free(cwd);
    
    // 提取最后一级目录名
    size_t pos = path.find_last_of("/\\");
    if (pos != std::string::npos) {
        return path.substr(pos + 1);
    }
    return path;
}

/**
 * @brief 解析 PS1 字符串
 * @param ps1 PS1 模板
 * @return 解析后的提示符
 */
std::string parsePS1(const std::string& ps1) {
    std::string result;
    std::string username = getUsername();
    std::string hostname = getHostname();
    std::string workdir = getCurrentDir();
    
    for (size_t i = 0; i < ps1.size(); ++i) {
        if (ps1[i] == '\\' && i + 1 < ps1.size()) {
            switch (ps1[i + 1]) {
                case 'u':  // 用户名
                    result += username;
                    i++;
                    break;
                case 'h':  // 主机名
                    result += hostname;
                    i++;
                    break;
                case 'W':  // 当前目录（最后一级）
                    result += workdir;
                    i++;
                    break;
                case 'w':  // 完整工作目录
                    result += getCurrentDir();
                    i++;
                    break;
                case '$':  // 普通用户为 $，root 为 #
#ifdef _WIN32
                    result += "$";
#else
                    result += (getuid() == 0) ? "#" : "$";
#endif
                    i++;
                    break;
                case '\\':  // 反斜杠
                    result += '\\';
                    i++;
                    break;
                default:
                    result += ps1[i];
                    break;
            }
        } else {
            result += ps1[i];
        }
    }
    
    return result;
}

/**
 * @brief 获取 PS1 提示符
 * @param executor 执行器
 * @return PS1 提示符
 */
std::string getPS1(wash::Executor& executor) {
    wash::Value ps1Value = executor.getEnvVariable("washPS1");
    std::string ps1;
    
    if (std::holds_alternative<std::string>(ps1Value)) {
        ps1 = std::get<std::string>(ps1Value);
    }
    
    if (ps1.empty()) {
        ps1 = DEFAULT_PS1;
    }
    
    return parsePS1(ps1);
}

/**
 * @brief 加载 rc 文件
 * @param path 文件路径
 * @param executor 执行器
 */
void loadRCFile(const std::string& path, wash::Executor& executor) {
    std::ifstream file(path);
    if (!file.is_open()) {
        return;
    }
    
    std::string line;
    std::string source;
    while (std::getline(file, line)) {
        source += line + "\n";
    }
    file.close();
    
    if (source.empty()) {
        return;
    }
    
    // 词法分析
    wash::Lexer lexer(source, path);
    auto tokens = lexer.tokenize();
    
    if (lexer.hasError()) {
        std::cerr << "词法错误: " << path << std::endl;
        return;
    }
    
    // 语法分析
    wash::Parser parser(tokens, path);
    auto ast = parser.parse();
    
    if (parser.hasError()) {
        std::cerr << parser.getError() << std::endl;
        return;
    }
    
    // 执行
    executor.execute(ast);
}

/**
 * @brief 加载 rc 文件（系统级和用户级）
 * @param executor 执行器
 * @param isLogin 是否为登录 shell
 */
void loadRCFiles(wash::Executor& executor, bool isLogin) {
    if (!isLogin) {
        return;
    }
    
    // 加载系统级 rc 文件
    loadRCFile("/etc/washrc", executor);
    
    // 加载用户级 rc 文件
    std::string homeDir = getHomeDir();
    loadRCFile(homeDir + "/.washrc", executor);
}

/**
 * @brief 保存历史记录
 * @param historyFile 历史记录文件路径
 * @param maxHistory 最大历史记录数
 */
void saveHistory(const std::string& historyFile, int maxHistory) {
    // 限制历史记录数量
    HIST_ENTRY** historyList = history_list();
    if (historyList) {
        int count = 0;
        while (historyList[count]) {
            count++;
        }
        
        if (count > maxHistory) {
            // 删除多余的历史记录
            int toRemove = count - maxHistory;
            for (int i = 0; i < toRemove; ++i) {
                remove_history(0);
            }
        }
    }
    
    write_history(historyFile.c_str());
}

/**
 * @brief 加载历史记录
 * @param historyFile 历史记录文件路径
 */
void loadHistory(const std::string& historyFile) {
    read_history(historyFile.c_str());
}

/**
 * @brief 执行源码字符串
 * @param source 源码
 * @param filename 文件名（用于错误报告）
 * @param executor 执行器
 * @return 退出码
 */
int executeSource(const std::string& source, const std::string& filename, 
                  wash::Executor& executor) {
    if (source.empty()) {
        return 0;
    }
    
    // 词法分析
    wash::Lexer lexer(source, filename);
    auto tokens = lexer.tokenize();
    
    if (lexer.hasError()) {
        std::cerr << "词法错误" << std::endl;
        return 1;
    }
    
    // 语法分析
    wash::Parser parser(tokens, filename);
    auto ast = parser.parse();
    
    if (parser.hasError()) {
        std::cerr << parser.getError() << std::endl;
        return 1;
    }
    
    // 执行
    wash::ExecResult result = executor.execute(ast);
    return executor.getExitCode();
}

/**
 * @brief 执行脚本文件
 * @param filename 文件名
 * @param executor 执行器
 * @return 退出码
 */
int executeFile(const std::string& filename, wash::Executor& executor) {
    std::ifstream file(filename);
    if (!file.is_open()) {
        std::cerr << "错误: 无法打开文件: " << filename << std::endl;
        return 1;
    }
    
    std::string source((std::istreambuf_iterator<char>(file)),
                       std::istreambuf_iterator<char>());
    file.close();
    
    return executeSource(source, filename, executor);
}

/**
 * @brief 交互模式主循环
 * @param executor 执行器
 */
void interactiveLoop(wash::Executor& executor) {
    std::string historyFile = getHomeDir() + "/.wash_history";
    
    // 获取历史记录最大条数
    int maxHistory = DEFAULT_HISTORY_MAX;
    wash::Value historyMaxValue = executor.getEnvVariable("HISTORY_MAX");
    if (std::holds_alternative<std::string>(historyMaxValue)) {
        try {
            maxHistory = std::stoi(std::get<std::string>(historyMaxValue));
        } catch (...) {
            // 使用默认值
        }
    }
    
    // 加载历史记录
    loadHistory(historyFile);
    
    // 设置 readline
    rl_attempted_completion_function = nullptr;
    
    std::cout << "wash - wbw121124's advanced shell" << std::endl;
    std::cout << "输入 'exit' 退出" << std::endl;
    std::cout << std::endl;
    
    while (!executor.shouldExit()) {
        std::string prompt = getPS1(executor);
        char* input = readline(prompt.c_str());
        
        if (!input) {
            // EOF
            std::cout << std::endl;
            break;
        }
        
        std::string line(input);
        free(input);
        
        // 跳过空行
        if (line.empty()) {
            continue;
        }
        
        // 添加到历史记录
        add_history(line.c_str());
        
        // 检查退出命令
        if (line == "exit") {
            break;
        }
        
        // 执行输入
        int exitCode = executeSource(line, "<input>", executor);
        
        // 保存历史记录
        saveHistory(historyFile, maxHistory);
    }
    
    // 保存历史记录
    saveHistory(historyFile, maxHistory);
}

/**
 * @brief 显示帮助信息
 */
void showHelp() {
    std::cout << "用法: wash [选项] [文件名]" << std::endl;
    std::cout << std::endl;
    std::cout << "选项:" << std::endl;
    std::cout << "  -c \"命令\"  执行命令后退出" << std::endl;
    std::cout << "  -l        作为登录 shell 启动" << std::endl;
    std::cout << "  -h        显示此帮助信息" << std::endl;
    std::cout << std::endl;
    std::cout << "示例:" << std::endl;
    std::cout << "  wash              交互模式" << std::endl;
    std::cout << "  wash script.wash  执行脚本" << std::endl;
    std::cout << "  wash -c \"echo hello\"  执行命令" << std::endl;
}

/**
 * @brief 主函数
 * @param argc 参数个数
 * @param argv 参数数组
 * @return 退出码
 */
int main(int argc, char const* argv[]) {
    wash::Executor executor;
    
    bool isLogin = false;
    std::string command;
    std::string filename;
    
    // 解析命令行参数
    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        
        if (arg == "-h" || arg == "--help") {
            showHelp();
            return 0;
        } else if (arg == "-l") {
            isLogin = true;
        } else if (arg == "-c") {
            if (i + 1 < argc) {
                command = argv[++i];
            } else {
                std::cerr << "错误: -c 选项需要参数" << std::endl;
                return 1;
            }
        } else if (arg[0] == '-') {
            std::cerr << "错误: 未知选项: " << arg << std::endl;
            showHelp();
            return 1;
        } else {
            if (filename.empty()) {
                filename = arg;
            } else {
                std::cerr << "错误: 文件名和 -c 命令不能同时使用" << std::endl;
                return 1;
            }
        }
    }
    
    // 加载 rc 文件
    loadRCFiles(executor, isLogin);
    
    // 根据参数决定执行模式
    if (!command.empty()) {
        // 执行命令模式
        return executeSource(command, "<command>", executor);
    } else if (!filename.empty()) {
        // 执行脚本模式
        return executeFile(filename, executor);
    } else {
        // 交互模式
        interactiveLoop(executor);
        return executor.getExitCode();
    }
}
