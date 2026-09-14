#!/bin/bash
export MSYSTEM=UCRT64
cd ~/home/wash/build
cmake -G "Unix Makefiles" .. 2>&1
make -j4 2>&1
