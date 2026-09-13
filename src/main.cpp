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
#include <unistd.h>
#include <libintl.h>
#include <locale.h>
#include <readline/readline.h>
#include <readline/history.h>

#include "lexer.h"
#include "parser.h"
#include "executor.h"

#define _(STRING) gettext(STRING)

static const char* DEFAULT_PS1 = "[\\u@\\h \\W]\\$ ";
static const int DEFAULT_HISTORY_MAX = 10000;

std::string getHomeDir() {
    const char* home = getenv("HOME");
    return home ? home : ".";
}

std::string getUsername() {
    const char* user = getenv("USER");
    return user ? user : "unknown";
}

std::string getHostname() {
    char hostname[256];
    gethostname(hostname, sizeof(hostname));
    return hostname;
}

std::string getCurrentDir() {
    char* cwd = getcwd(nullptr, 0);
    if (!cwd) return ".";
    std::string path(cwd);
    free(cwd);
    size_t pos = path.find_last_of("/");
    if (pos != std::string::npos) return path.substr(pos + 1);
    return path;
}

std::string parsePS1(const std::string& ps1) {
    std::string result;
    std::string username = getUsername();
    std::string hostname = getHostname();
    std::string workdir = getCurrentDir();
    
    for (size_t i = 0; i < ps1.size(); ++i) {
        if (ps1[i] == '\\' && i + 1 < ps1.size()) {
            switch (ps1[i + 1]) {
                case 'u': result += username; i++; break;
                case 'h': result += hostname; i++; break;
                case 'W': result += workdir; i++; break;
                case 'w': result += getCurrentDir(); i++; break;
                case '$': result += (getuid() == 0) ? "#" : "$"; i++; break;
                case '\\': result += '\\'; i++; break;
                default: result += ps1[i]; break;
            }
        } else {
            result += ps1[i];
        }
    }
    return result;
}

std::string getPS1(wash::Executor& executor) {
    wash::Value ps1Value = executor.getEnvVariable("washPS1");
    std::string ps1;
    if (std::holds_alternative<std::string>(ps1Value)) {
        ps1 = std::get<std::string>(ps1Value);
    }
    if (ps1.empty()) ps1 = DEFAULT_PS1;
    return parsePS1(ps1);
}

void loadRCFile(const std::string& path, wash::Executor& executor) {
    std::ifstream file(path);
    if (!file.is_open()) return;
    
    std::string line, source;
    while (std::getline(file, line)) source += line + "\n";
    file.close();
    if (source.empty()) return;
    
    wash::Lexer lexer(source, path);
    auto tokens = lexer.tokenize();
    if (lexer.hasError()) { std::cerr << "词法错误: " << path << std::endl; return; }
    
    wash::Parser parser(tokens, path);
    auto ast = parser.parse();
    if (parser.hasError()) { std::cerr << parser.getError() << std::endl; return; }
    
    executor.execute(ast);
}

void loadRCFiles(wash::Executor& executor, bool isLogin) {
    if (!isLogin) return;
    loadRCFile("/etc/washrc", executor);
    loadRCFile(getHomeDir() + "/.washrc", executor);
}

void saveHistory(const std::string& historyFile, int maxHistory) {
    HIST_ENTRY** historyList = history_list();
    if (historyList) {
        int count = 0;
        while (historyList[count]) count++;
        if (count > maxHistory) {
            int toRemove = count - maxHistory;
            for (int i = 0; i < toRemove; ++i) remove_history(0);
        }
    }
    write_history(historyFile.c_str());
}

void loadHistory(const std::string& historyFile) {
    read_history(historyFile.c_str());
}

int executeSource(const std::string& source, const std::string& filename, wash::Executor& executor) {
    if (source.empty()) return 0;
    
    wash::Lexer lexer(source, filename);
    auto tokens = lexer.tokenize();
    if (lexer.hasError()) { std::cerr << "词法错误" << std::endl; return 1; }
    
    wash::Parser parser(tokens, filename);
    auto ast = parser.parse();
    if (parser.hasError()) { std::cerr << parser.getError() << std::endl; return 1; }
    
    wash::ExecResult result = executor.execute(ast);
    return executor.getExitCode();
}

int executeFile(const std::string& filename, wash::Executor& executor) {
    std::ifstream file(filename);
    if (!file.is_open()) { std::cerr << "错误: 无法打开文件: " << filename << std::endl; return 1; }
    
    std::string source((std::istreambuf_iterator<char>(file)), std::istreambuf_iterator<char>());
    file.close();
    return executeSource(source, filename, executor);
}

bool isIncomplete(const std::string& input) {
    int braces = 0, parens = 0, brackets = 0;
    bool inSingleQuote = false, inDoubleQuote = false, inBacktick = false;
    
    for (size_t i = 0; i < input.size(); ++i) {
        char c = input[i];
        
        // 转义字符跳过下一个字符
        if (c == '\\' && !inSingleQuote && i + 1 < input.size()) {
            ++i;
            continue;
        }
        
        // 引号状态切换
        if (c == '\'' && !inDoubleQuote && !inBacktick) {
            inSingleQuote = !inSingleQuote;
        } else if (c == '"' && !inSingleQuote && !inBacktick) {
            inDoubleQuote = !inDoubleQuote;
        } else if (c == '`' && !inSingleQuote && !inDoubleQuote) {
            inBacktick = !inBacktick;
        }
        
        // 引号内不计数
        if (inSingleQuote || inDoubleQuote || inBacktick) continue;
        
        // 括号计数
        if (c == '{') ++braces;
        else if (c == '}') --braces;
        else if (c == '(') ++parens;
        else if (c == ')') --parens;
        else if (c == '[') ++brackets;
        else if (c == ']') --brackets;
    }
    
    // 未闭合则不完整
    return braces > 0 || parens > 0 || brackets > 0;
}

void interactiveLoop(wash::Executor& executor) {
    std::string historyFile = getHomeDir() + "/.wash_history";
    
    int maxHistory = DEFAULT_HISTORY_MAX;
    wash::Value historyMaxValue = executor.getEnvVariable("HISTORY_MAX");
    if (std::holds_alternative<std::string>(historyMaxValue)) {
        try { maxHistory = std::stoi(std::get<std::string>(historyMaxValue)); } catch (...) {}
    }
    
    loadHistory(historyFile);
    rl_attempted_completion_function = nullptr;
    
    std::cout << "wash - wbw121124's advanced shell" << std::endl;
    std::cout << "输入 'exit' 退出" << std::endl;
    std::cout << std::endl;
    
    while (!executor.shouldExit()) {
        std::string prompt = getPS1(executor);
        char* input = readline(prompt.c_str());
        if (!input) { std::cout << std::endl; break; }
        
        std::string line(input);
        free(input);
        if (line.empty()) continue;
        
        // 多行输入：检查未闭合的括号/引号
        while (isIncomplete(line)) {
            char* continuation = readline("> ");
            if (!continuation) break;
            std::string contLine(continuation);
            free(continuation);
            line += "\n" + contLine;
            if (contLine.empty()) break;
        }
        
        add_history(line.c_str());
        if (line == "exit") break;
        
        executeSource(line, "<input>", executor);
        saveHistory(historyFile, maxHistory);
    }
    
    saveHistory(historyFile, maxHistory);
}

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

int main(int argc, char const* argv[]) {
    // 初始化 i18n
    setlocale(LC_ALL, "");
    bindtextdomain("wash", "share/locale");
    textdomain("wash");
    
    wash::Executor executor;
    bool isLogin = false;
    std::string command, filename;
    
    // 传递原始命令行参数给 executor
    executor.setScriptArgs(argc, argv);
    
    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "-h" || arg == "--help") { showHelp(); return 0; }
        else if (arg == "-l") isLogin = true;
        else if (arg == "-c") {
            if (i + 1 < argc) command = argv[++i];
            else { std::cerr << "错误: -c 选项需要参数" << std::endl; return 1; }
        } else if (arg[0] == '-') {
            std::cerr << "错误: 未知选项: " << arg << std::endl;
            showHelp(); return 1;
        } else {
            if (filename.empty()) filename = arg;
            else { std::cerr << "错误: 文件名和 -c 命令不能同时使用" << std::endl; return 1; }
        }
    }
    
    loadRCFiles(executor, isLogin);
    
    if (!command.empty()) return executeSource(command, "<command>", executor);
    else if (!filename.empty()) return executeFile(filename, executor);
    else { interactiveLoop(executor); return executor.getExitCode(); }
}
