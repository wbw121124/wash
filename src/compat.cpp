/**
 * @file compat.cpp
 * @brief wash 跨平台兼容层实现
 * 
 * @author wash
 * @date 2026-09-13
 */

#include "compat.h"
#include <cstring>
#include <algorithm>
#include <fstream>
#include <sstream>

#if defined(_WIN32) && !defined(__MSYS__)
    #include <share.h>
    #include <windows.h>
#endif

namespace wash {
namespace compat {

// ==================== 进程管理 ====================

std::string execCommand(const std::string& cmd) {
    std::string result;
    FILE* pipe = popenCommand(cmd, "r");
    if (!pipe) return result;
    
    char buf[4096];
    while (fgets(buf, sizeof(buf), pipe)) {
        result += buf;
    }
    pcloseCommand(pipe);
    return result;
}

std::string execCommand(const std::vector<std::string>& args, bool captureOutput) {
    if (args.empty()) return "";
    
    std::string cmd = args[0];
    for (size_t i = 1; i < args.size(); ++i) {
        cmd += " " + args[i];
    }
    return execCommand(cmd);
}

FILE* popenCommand(const std::string& command, const std::string& mode) {
    return popen(command.c_str(), mode.c_str());
}

int pcloseCommand(FILE* pipe) {
    if (!pipe) return -1;
#if defined(WASH_NATIVE_WIN32)
    return _pclose(pipe);
#else
    return pclose(pipe);
#endif
}

// ==================== 文件操作 ====================

int dupFd(int oldfd) {
#ifdef _WIN32
    return _dup(oldfd);
#else
    return dup(oldfd);
#endif
}

int dup2Fd(int oldfd, int newfd) {
#ifdef _WIN32
    return _dup2(oldfd, newfd);
#else
    return dup2(oldfd, newfd);
#endif
}

int closeFd(int fd) {
#ifdef _WIN32
    return _close(fd);
#else
    return close(fd);
#endif
}

int fileOpen(const std::string& path, int flags, int mode) {
#ifdef _WIN32
    int fd;
    _sopen_s(&fd, path.c_str(), flags | _O_BINARY, _SH_DENYNO, mode);
    return fd;
#else
    return open(path.c_str(), flags, mode);
#endif
}

ssize_t fileRead(int fd, void* buf, size_t count) {
#ifdef _WIN32
    return _read(fd, buf, static_cast<unsigned int>(count));
#else
    return read(fd, buf, count);
#endif
}

ssize_t fileWrite(int fd, const void* buf, size_t count) {
#ifdef _WIN32
    return _write(fd, buf, static_cast<unsigned int>(count));
#else
    return write(fd, buf, count);
#endif
}

bool fileExists(const std::string& path) {
#if defined(WASH_NATIVE_WIN32)
    return GetFileAttributesA(path.c_str()) != INVALID_FILE_ATTRIBUTES;
#else
    return access(path.c_str(), F_OK) == 0;
#endif
}

std::string readFile(const std::string& path) {
    std::ifstream file(path, std::ios::binary);
    if (!file.is_open()) return "";
    
    std::ostringstream content;
    content << file.rdbuf();
    return content.str();
}

bool writeFile(const std::string& path, const std::string& content, bool append) {
    std::ios::openmode mode = std::ios::binary;
    mode |= append ? std::ios::app : std::ios::trunc;
    mode |= std::ios::out;
    
    std::ofstream file(path, mode);
    if (!file.is_open()) return false;
    
    file << content;
    return file.good();
}

// ==================== 环境变量 ====================

int setEnvVar(const std::string& name, const std::string& value) {
#ifdef _WIN32
    std::string env = name + "=" + value;
    return _putenv(env.c_str());
#else
    return setenv(name.c_str(), value.c_str(), 1);
#endif
}

std::string getEnvVar(const std::string& name, const std::string& defaultValue) {
#if defined(WASH_NATIVE_WIN32)
    char buf[32768];
    DWORD len = GetEnvironmentVariableA(name.c_str(), buf, sizeof(buf));
    if (len == 0 && GetLastError() == ERROR_ENVVAR_NOT_FOUND) {
        return defaultValue;
    }
    return std::string(buf, len);
#else
    const char* val = getenv(name.c_str());
    return val ? std::string(val) : defaultValue;
#endif
}

int unsetEnvVar(const std::string& name) {
#if defined(_WIN32)
    // UCRT64/MinGW64 没有 unsetenv()，用 _putenv 设空值模拟
    std::string env = name + "=";
    return _putenv(env.c_str());
#else
    return unsetenv(name.c_str());
#endif
}

// ==================== 系统信息 ====================

std::string getHostname() {
#ifdef _WIN32
    const char* host = getenv("COMPUTERNAME");
    if (host) return host;
#else
    char buf[256] = {0};
    if (gethostname(buf, sizeof(buf) - 1) == 0) {
        return buf;
    }
#endif
    return "unknown";
}

std::string getUsername() {
#ifdef _WIN32
    // UCRT64/MinGW64 没有 getpwuid/getuid，用 USERNAME 环境变量
    const char* user = getenv("USERNAME");
    if (user) return user;
    return "unknown";
#else
    const char* user = getenv("USER");
    if (user) return user;
    
    struct passwd* pw = getpwuid(getuid());
    if (pw) return pw->pw_name;
    return "unknown";
#endif
}

int getUserId() {
#ifdef _WIN32
    return 0;
#else
    return getuid();
#endif
}

std::string getCurrentDir() {
    char buf[4096] = {0};
#ifdef _WIN32
    if (_getcwd(buf, sizeof(buf))) {
        return buf;
    }
#else
    if (getcwd(buf, sizeof(buf))) {
        return buf;
    }
#endif
    return ".";
}

int changeDir(const std::string& path) {
#ifdef _WIN32
    return _chdir(path.c_str());
#else
    return chdir(path.c_str());
#endif
}

std::string getExePath() {
#ifdef _WIN32
    char buf[MAX_PATH] = {0};
    // UCRT64/MinGW64 有 GetModuleFileNameA（需链接 kernel32）
    // 如果不可用则返回空
    DWORD len = GetModuleFileNameA(NULL, buf, MAX_PATH);
    if (len > 0) return buf;
    return "./wash";
#else
    char buf[1024] = {0};
    ssize_t len = readlink("/proc/self/exe", buf, sizeof(buf) - 1);
    if (len > 0) {
        buf[len] = '\0';
        return buf;
    }
    return "./wash";
#endif
}

std::string getExeDir() {
    std::string path = getExePath();
    size_t lastSlash = path.find_last_of("/\\");
    if (lastSlash != std::string::npos) {
        return path.substr(0, lastSlash);
    }
    return ".";
}

// ==================== 路径操作 ====================

std::string normalizePath(const std::string& path) {
    std::string result = path;
#if defined(_WIN32) && !defined(WASH_MSYS2)
    std::replace(result.begin(), result.end(), '/', '\\');
#else
    std::replace(result.begin(), result.end(), '\\', '/');
#endif
    return result;
}

std::string dirname(const std::string& path) {
    size_t lastSlash = path.find_last_of("/\\");
    if (lastSlash != std::string::npos) {
        return path.substr(0, lastSlash);
    }
    return ".";
}

std::string basename(const std::string& path) {
    size_t lastSlash = path.find_last_of("/\\");
    if (lastSlash != std::string::npos) {
        return path.substr(lastSlash + 1);
    }
    return path;
}

std::string joinPath(const std::string& base, const std::string& relative) {
    if (base.empty()) return relative;
    if (relative.empty()) return base;
    
    char lastChar = base.back();
    if (lastChar == '/' || lastChar == '\\') {
        return base + relative;
    }
    return base + DIR_SEPARATOR + relative;
}

// ==================== 字符串工具 ====================

std::string trim(const std::string& str) {
    size_t start = str.find_first_not_of(" \t\n\r");
    if (start == std::string::npos) return "";
    
    size_t end = str.find_last_not_of(" \t\n\r");
    return str.substr(start, end - start + 1);
}

std::vector<std::string> split(const std::string& str, char delimiter) {
    std::vector<std::string> result;
    std::istringstream stream(str);
    std::string token;
    
    while (std::getline(stream, token, delimiter)) {
        result.push_back(token);
    }
    
    return result;
}

std::string join(const std::vector<std::string>& parts, const std::string& delimiter) {
    std::string result;
    for (size_t i = 0; i < parts.size(); ++i) {
        if (i > 0) result += delimiter;
        result += parts[i];
    }
    return result;
}

std::string toLower(const std::string& str) {
    std::string result = str;
    std::transform(result.begin(), result.end(), result.begin(),
        [](unsigned char c) { return std::tolower(c); });
    return result;
}

std::string toUpper(const std::string& str) {
    std::string result = str;
    std::transform(result.begin(), result.end(), result.begin(),
        [](unsigned char c) { return std::toupper(c); });
    return result;
}

} // namespace compat
} // namespace wash
