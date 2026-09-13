/**
 * @file color.cpp
 * @brief wash 终端颜色支持实现
 * 
 * @author wash
 * @date 2026-09-13
 */

#include "color.h"
#include <unistd.h>

extern "C" {
#include <term.h>
#include <curses.h>
}

namespace wash {

ColorManager::ColorManager() : initialized_(false), hasColor_(false),
    setaf_(nullptr), setab_(nullptr), sgr0_(nullptr) {
    int err = 0;
    if (setupterm(nullptr, STDOUT_FILENO, &err) == OK) {
        initialized_ = true;
        setaf_ = tigetstr(const_cast<char*>("setaf"));
        setab_ = tigetstr(const_cast<char*>("setab"));
        sgr0_ = tigetstr(const_cast<char*>("sgr0"));
        
        // 检查是否支持颜色
        if (setaf_ && setab_ && tigetnum(const_cast<char*>("colors")) >= 8) {
            hasColor_ = true;
        }
    }
}

ColorManager::~ColorManager() {
    if (initialized_) {
        del_curterm(cur_term);
    }
}

bool ColorManager::hasColor() const {
    return hasColor_;
}

void ColorManager::setFg(Color color) const {
    if (hasColor_ && setaf_) {
        tputs(tigetstr(const_cast<char*>("setaf")), 1, putchar);
    }
}

void ColorManager::setBg(Color color) const {
    if (hasColor_ && setab_) {
        tputs(tigetstr(const_cast<char*>("setab")), 1, putchar);
    }
}

void ColorManager::reset() const {
    if (hasColor_ && sgr0_) {
        tputs(sgr0_, 1, putchar);
    }
}

std::string ColorManager::fgStr(Color color) const {
    if (!hasColor_) return "";
    
    // 使用 ANSI 转义序列作为后备
    int colorNum = static_cast<int>(color);
    return "\033[3" + std::to_string(colorNum) + "m";
}

std::string ColorManager::resetStr() const {
    if (!hasColor_) return "";
    return "\033[0m";
}

} // namespace wash
