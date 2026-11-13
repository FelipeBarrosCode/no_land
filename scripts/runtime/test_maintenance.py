import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('maintenance', Path(__file__).with_name('maintain-runtime.py'))
maintenance = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(maintenance)
CATALOG = json.loads((Path(__file__).resolve().parents[2] / 'runtime-images/ubuntu24/prerequisites.json').read_text())


class MaintenanceTests(unittest.TestCase):
    def test_arguments_accept_only_catalog_packages_and_immutable_system_ids(self):
        self.assertEqual(maintenance.arguments(['install', 'ubuntu24-fixture', 'libopenal1'], CATALOG),
                         ('install', 'ubuntu24-fixture', ['libopenal1']))
        for args in [[], ['update'], ['update', '../../etc'], ['update', 'ubuntu24-ok', 'libopenal1'],
                     ['install', 'ubuntu24-ok'], ['install', 'ubuntu24-ok', '--allow-unauthenticated'],
                     ['install', 'ubuntu24-ok', 'libopenal1;touch owned'],
                     ['install', 'ubuntu24-ok', '/tmp/app'], ['install', 'ubuntu24-ok', 'nvidia-driver-580']]:
            with self.subTest(args=args), self.assertRaises(ValueError):
                maintenance.arguments(args, CATALOG)

    def fixture(self, fail=False, changed=True):
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        images = root / 'images'
        config = root / 'config'
        source = images / 'ubuntu24-original'
        source.mkdir(parents=True)
        config.mkdir()
        (source / 'etc/apt/sources.list.d').mkdir(parents=True)
        (source / 'usr/sbin').mkdir(parents=True)
        (source / 'etc/noland-runtime-complete').write_text('old-receipt')
        (source / 'original-file').write_text('unchanged')
        (config / 'image.json').write_text('{"id":"ubuntu24-original"}')
        self.enterContext(patch.object(maintenance, 'IMAGES', images))
        self.enterContext(patch.object(maintenance, 'CONFIG', config))
        self.enterContext(patch.object(maintenance, 'protected'))
        self.enterContext(patch.object(maintenance, 'validate_image'))
        self.enterContext(patch.object(maintenance.shutil, 'disk_usage', return_value=shutil._ntuple_diskusage(10**12, 0, 10**12)))
        self.enterContext(patch.object(maintenance, 'inventory', side_effect=lambda path, _: b'old-packages' if path == source or not changed else b'new-packages'))
        commands = []

        def run(args, log, capture=False, timeout=1800):
            commands.append(args)
            if args[0] == '/usr/bin/du':
                return b'4096 image'
            if args[0] == '/bin/cp':
                shutil.copytree(source, Path(args[-1]), dirs_exist_ok=True)
            if '/usr/bin/apt-get' in args and ('install' in args or 'upgrade' in args) and fail:
                raise RuntimeError('synthetic package failure')
            return b''

        self.enterContext(patch.object(maintenance, 'run', side_effect=run))
        return root, images, config, source, commands

    def test_success_publishes_new_image_and_preserves_original_and_unrelated_state(self):
        root, images, config, source, commands = self.fixture()
        unrelated = images / '.other-owned-stage'
        unrelated.mkdir()
        result = maintenance.maintain('install', source.name, ['libopenal1'], None)
        self.assertTrue(result['changed'])
        self.assertEqual((source / 'original-file').read_text(), 'unchanged')
        self.assertEqual((source / 'etc/noland-runtime-complete').read_text(), 'old-receipt')
        self.assertTrue((images / result['imageId'] / 'original-file').is_file())
        self.assertEqual(json.loads((config / 'image.json').read_text())['previousId'], source.name)
        self.assertTrue(unrelated.is_dir())
        self.assertFalse(list(images.glob('.maintenance-*')))
        for args in commands:
            if '/usr/bin/apt-get' in args:
                self.assertEqual(args[0], '/usr/sbin/chroot')
                self.assertIn(str(images) + '/.maintenance-', args[1])
                self.assertNotIn('--allow-unauthenticated', args)

    def test_failed_download_or_install_never_activates_or_mutates_source(self):
        _, images, config, source, _ = self.fixture(fail=True)
        receipt = (config / 'image.json').read_bytes()
        with self.assertRaises(RuntimeError):
            maintenance.maintain('install', source.name, ['libopenal1'], None)
        self.assertEqual((config / 'image.json').read_bytes(), receipt)
        self.assertEqual((source / 'original-file').read_text(), 'unchanged')
        self.assertEqual(list(images.iterdir()), [source])

    def test_unchanged_update_does_not_publish_another_image(self):
        _, images, config, source, _ = self.fixture(changed=False)
        receipt = (config / 'image.json').read_bytes()
        result = maintenance.maintain('update', source.name, [], None)
        self.assertEqual(result, {'imageId': source.name, 'changed': False})
        self.assertEqual((config / 'image.json').read_bytes(), receipt)
        self.assertEqual(list(images.iterdir()), [source])


if __name__ == '__main__':
    unittest.main()
