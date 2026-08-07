import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin



class GoodPlugin(Plugin):
    pass


if __name__ == "__main__":
    GoodPlugin().run()
