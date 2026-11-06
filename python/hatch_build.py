"""Tags the wheel for Linux x86_64 with glibc 2.17 (manylinux2014), the binary's baseline."""

from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class CustomBuildHook(BuildHookInterface):
    def initialize(self, version, build_data):
        build_data["pure_python"] = False
        build_data["tag"] = "py3-none-manylinux2014_x86_64"
