import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-09-banner"

    def on_initialize(self, req_id, params):
        sys.stdout.buffer.write(b"hello world\n")  # stdout 混入非 JSON -> BEH-09
        sys.stdout.buffer.flush()
        self.reply(req_id, {"id": self.plugin_id, "name": self.plugin_name,
                            "version": self.plugin_version,
                            "capabilities": {"annotate": False, "subscribe": False,
                                             "binary_sidecar": False}})

if __name__ == "__main__":
    Plugin().run()
