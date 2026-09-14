/**
 * @file color.cpp
 * @brief wash 终端颜色支持实现
 * 
 * 纯 ANSI 转义码实现，不依赖 ncurses/terminfo。
 * 
 * @author wash
 * @date 2026-09-13
 */

#include "color.h"
#include <cstdlib>
#include <cstring>

namespace wash {

ColorManager::ColorManager() : hasColor_(false) {
    const char* term = getenv("TERM");
    if (term && std::strcmp(term, "dumb") != 0) {
        hasColor_ = true;
    }
    // MSYS2/UCRT64/MinGW 终端都支持 ANSI，只要不是 dumb
    if (!term) {
        hasColor_ = true;
    }
}

bool ColorManager::hasColor() const {
    return hasColor_;
}

static std::string ansiFg(Color color) {
    int code = static_cast<int>(color);
    if (code == static_cast<int>(Color::DEFAULT)) {
        return "\033[39m";
    }
    return "\033[3" + std::to_string(code) + "m";
}

static std::string ansiBg(Color color) {
    int code = static_cast<int>(color);
    if (code == static_cast<int>(Color::DEFAULT)) {
        return "\033[49m";
    }
    return "\033[4" + std::to_string(code) + "m";
}

void ColorManager::setFg(Color color) const {
    if (hasColor_) {
        std::string seq = ansiFg(color);
        fputs(seq.c_str(), stdout);
    }
}

void ColorManager::setBg(Color color) const {
    if (hasColor_) {
        std::string seq = ansiBg(color);
        fputs(seq.c_str(), stdout);
    }
}

void ColorManager::reset() const {
    if (hasColor_) {
        fputs("\033[0m", stdout);
    }
}

std::string ColorManager::fgStr(Color color) const {
    if (!hasColor_) return "";
    return ansiFg(color);
}

std::string ColorManager::resetStr() const {
    if (!hasColor_) return "";
    return "\033[0m";
}

} // namespace wash
