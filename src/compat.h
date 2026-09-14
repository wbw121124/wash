/**
 * @file compat.h
 * @brief wash 跨平台兼容层
 * 
 * 统一 POSIX 和 Windows/MSYS2/UCRT64/MinGW 的 API 差异。
 * 
 * @author wash
 * @date 2026-09-13
 */

#ifndef WASH_COMPAT_H
#define WASH_COMPAT_H

#include <string>
#include <vector>
#include <cstdio>

/**
 * @def WASH_NATIVE_WIN32
 * @brief 标记原生 Windows（MSVC）编译环境
 *
 * 在 MSYS2（UCRT64/MinGW64/MSYS）中虽然 `_WIN32` 被编译器自动定义，
 * 但 MSYS2 提供了完整的 POSIX 兼容层，不应包含 `<windows.h>`，
 * 否则会导致 MSYS2 的 POSIX 头文件与 UCRT 头文件严重冲突。
 *
 * 因此此处将检测顺序调整为：
 *   1. `__MINGW32__` / `__MINGW64__` / `__MSYS__` → MSYS2 环境，走 POSIX 路径
 *   2. `_WIN32`（且非 MinGW） → 原生 Windows MSVC，走 Win32 API 路径
 *   3. 其他 → 纯 POSIX（Linux / macOS / BSD）
 */
#if defined(__MSYS__)
    // ========== MSYS 子系统（/usr/bin/gcc，完整 POSIX 兼容）==========
    #include <unistd.h>
    #include <sys/wait.h>
    #include <sys/types.h>
    #include <fcntl.h>
    #include <pwd.h>

    #define WASH_MSYS2 1

#elif defined(__MINGW32__) || defined(__MINGW64__)
    // ========== UCRT64 / MinGW64 ==========
    // 注意：CMake 已从隐式搜索路径中移除 /usr/include（MSYS），确保此处的
    // <fcntl.h>/<io.h>/<stdio.h> 等均来自 /ucrt64/include（UCRT64），
    // 不会引入 MSYS 的冲突头文件。
    #include <io.h>
    #include <direct.h>
    #include <fcntl.h>
    #include <stdio.h>

    // 文件访问权限检查常量
    #define F_OK 0
    #define W_OK 2
    #define R_OK 4

    // UCRT64 POSIX 兼容宏映射（<fcntl.h> 已定义 O_RDONLY 等）
    #define popen _popen
    #define pclose _pclose
    #define getcwd _getcwd
    #define chdir _chdir
    #define access _access
    #define fileno _fileno

    #define WASH_MSYS2 1

#elif defined(_WIN32)
    // ========== 原生 Windows（MSVC）==========
    #ifndef WIN32_LEAN_AND_MEAN
        #define WIN32_LEAN_AND_MEAN
    #endif
    #include <windows.h>
    #include <io.h>
    #include <direct.h>
    #include <process.h>

    // 文件操作标志（MSVC 使用 _ 前缀）
    #ifndef O_RDONLY
        #define O_RDONLY    _O_RDONLY
        #define O_WRONLY    _O_WRONLY
        #define O_RDWR      _O_RDWR
        #define O_CREAT     _O_CREAT
        #define O_TRUNC     _O_TRUNC
        #define O_APPEND    _O_APPEND
        #define O_EXCL      _O_EXCL
    #endif

    // 文件权限（MSVC 使用 _ 前缀）
    #ifndef S_IRUSR
        #define S_IRUSR _S_IREAD
        #define S_IWUSR _S_IWRITE
    #endif

    typedef int pid_t;
    typedef int ssize_t;

    #define popen _popen
    #define pclose _pclose
    #define getcwd _getcwd
    #define chdir _chdir
    #define access _access
    #define fileno _fileno

    #define F_OK 0
    #define W_OK 2
    #define R_OK 4

    #define WASH_NATIVE_WIN32 1

#else
    // ========== 纯 POSIX（Linux / macOS / BSD）==========
    #include <unistd.h>
    #include <sys/wait.h>
    #include <sys/types.h>
    #include <fcntl.h>
    #include <pwd.h>
#endif

namespace wash {
namespace compat {

// ==================== 进程管理 ====================

/**
 * @brief 执行外部命令并获取输出
 * @param cmd 命令字符串
 * @return 命令输出
 */
std::string execCommand(const std::string& cmd);

/**
 * @brief 执行外部命令（带参数）
 * @param args 命令参数列表
 * @param captureOutput 是否捕获输出
 * @return 命令输出（如果 captureOutput=true），否则返回空字符串
 */
std::string execCommand(const std::vector<std::string>& args, bool captureOutput = true);

/**
 * @brief popen 封装
 * @param command 命令
 * @param mode 模式 ("r" 或 "w")
 * @return FILE 指针
 */
FILE* popenCommand(const std::string& command, const std::string& mode = "r");

/**
 * @brief pclose 封装
 * @param pipe FILE 指针
 * @return 退出状态
 */
int pcloseCommand(FILE* pipe);

// ==================== 文件操作 ====================

/**
 * @brief 复制文件描述符
 * @param oldfd 旧文件描述符
 * @return 新文件描述符，失败返回 -1
 */
int dupFd(int oldfd);

/**
 * @brief 复制文件描述符到指定位置
 * @param oldfd 旧文件描述符
 * @param newfd 新文件描述符
 * @return 新文件描述符，失败返回 -1
 */
int dup2Fd(int oldfd, int newfd);

/**
 * @brief 关闭文件描述符
 * @param fd 文件描述符
 * @return 0 成功，-1 失败
 */
int closeFd(int fd);

/**
 * @brief 文件打开标志常量
 */
#ifdef _O_RDONLY
    constexpr int FDC_O_RDONLY = _O_RDONLY;
    constexpr int FDC_O_WRONLY = _O_WRONLY;
    constexpr int FDC_O_RDWR   = _O_RDWR;
    constexpr int FDC_O_CREAT  = _O_CREAT;
    constexpr int FDC_O_TRUNC  = _O_TRUNC;
    constexpr int FDC_O_APPEND = _O_APPEND;
#else
    constexpr int FDC_O_RDONLY = O_RDONLY;
    constexpr int FDC_O_WRONLY = O_WRONLY;
    constexpr int FDC_O_RDWR   = O_RDWR;
    constexpr int FDC_O_CREAT  = O_CREAT;
    constexpr int FDC_O_TRUNC  = O_TRUNC;
    constexpr int FDC_O_APPEND = O_APPEND;
#endif

/**
 * @brief 打开文件
 * @param path 文件路径
 * @param flags 打开标志 (FDC_O_WRONLY, FDC_O_CREAT, etc.)
 * @param mode 文件权限 (可选)
 * @return 文件描述符，失败返回 -1
 */
int fileOpen(const std::string& path, int flags, int mode = 0644);

/**
 * @brief 读取文件
 * @param fd 文件描述符
 * @param buf 缓冲区
 * @param count 要读取的字节数
 * @return 实际读取的字节数
 */
ssize_t fileRead(int fd, void* buf, size_t count);

/**
 * @brief 写入文件
 * @param fd 文件描述符
 * @param buf 缓冲区
 * @param count 要写入的字节数
 * @return 实际写入的字节数
 */
ssize_t fileWrite(int fd, const void* buf, size_t count);

/**
 * @brief 检查文件是否存在
 * @param path 文件路径
 * @return true 存在，false 不存在
 */
bool fileExists(const std::string& path);

/**
 * @brief 读取文件全部内容
 * @param path 文件路径
 * @return 文件内容
 */
std::string readFile(const std::string& path);

/**
 * @brief 写入文件
 * @param path 文件路径
 * @param content 文件内容
 * @param append 是否追加模式
 * @return true 成功，false 失败
 */
bool writeFile(const std::string& path, const std::string& content, bool append = false);

// ==================== 环境变量 ====================

/**
 * @brief 设置环境变量
 * @param name 变量名
 * @param value 变量值
 * @return 0 成功，非 0 失败
 */
int setEnvVar(const std::string& name, const std::string& value);

/**
 * @brief 获取环境变量
 * @param name 变量名
 * @param defaultValue 默认值
 * @return 变量值，不存在返回默认值
 */
std::string getEnvVar(const std::string& name, const std::string& defaultValue = "");

/**
 * @brief 删除环境变量
 * @param name 变量名
 * @return 0 成功，非 0 失败
 */
int unsetEnvVar(const std::string& name);

// ==================== 系统信息 ====================

/**
 * @brief 获取主机名
 * @return 主机名
 */
std::string getHostname();

/**
 * @brief 获取用户名
 * @return 用户名
 */
std::string getUsername();

/**
 * @brief 获取用户 ID
 * @return 用户 ID (Windows 下始终返回 0)
 */
int getUserId();

/**
 * @brief 获取当前工作目录
 * @return 当前工作目录
 */
std::string getCurrentDir();

/**
 * @brief 切换工作目录
 * @param path 目标目录
 * @return 0 成功，-1 失败
 */
int changeDir(const std::string& path);

/**
 * @brief 获取可执行文件路径
 * @return 可执行文件路径
 */
std::string getExePath();

/**
 * @brief 获取可执行文件所在目录
 * @return 可执行文件目录
 */
std::string getExeDir();

// ==================== 路径操作 ====================

/**
 * @brief 路径分隔符
 */
#if defined(_WIN32) && !defined(WASH_MSYS2)
    constexpr char DIR_SEPARATOR = '\\';
    constexpr char DIR_SEPARATOR_ALT = '/';
#else
    constexpr char DIR_SEPARATOR = '/';
    constexpr char DIR_SEPARATOR_ALT = '\\';
#endif

/**
 * @brief 规范化路径分隔符
 * @param path 路径
 * @return 规范化后的路径
 */
std::string normalizePath(const std::string& path);

/**
 * @brief 获取路径的目录部分
 * @param path 路径
 * @return 目录部分
 */
std::string dirname(const std::string& path);

/**
 * @brief 获取路径的文件名部分
 * @param path 路径
 * @return 文件名部分
 */
std::string basename(const std::string& path);

/**
 * @brief 连接路径
 * @param base 基础路径
 * @param relative 相对路径
 * @return 连接后的路径
 */
std::string joinPath(const std::string& base, const std::string& relative);

// ==================== 字符串工具 ====================

/**
 * @brief 去除字符串首尾空白
 * @param str 输入字符串
 * @return 去除空白后的字符串
 */
std::string trim(const std::string& str);

/**
 * @brief 分割字符串
 * @param str 输入字符串
 * @param delimiter 分隔符
 * @return 分割后的字符串列表
 */
std::vector<std::string> split(const std::string& str, char delimiter);

/**
 * @brief 连接字符串
 * @param parts 字符串列表
 * @param delimiter 分隔符
 * @return 连接后的字符串
 */
std::string join(const std::vector<std::string>& parts, const std::string& delimiter);

/**
 * @brief 大小写转换
 * @param str 输入字符串
 * @return 转换后的字符串
 */
std::string toLower(const std::string& str);
std::string toUpper(const std::string& str);

} // namespace compat
} // namespace wash

#endif // WASH_COMPAT_H
