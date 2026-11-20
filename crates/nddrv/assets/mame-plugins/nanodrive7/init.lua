local exports = { name = 'nanodrive7', version = '0.2.1', description = 'MAME sound registers to ND7 bridge', license = 'BSD-3-Clause', author = { name = 'ND7' } }
local subscriptions = {}
local taps, socket, active, paused = {}, nil, false, false

local chip_types = {
    ym2203 = { id = 0x55, family = 'opn' },
    ym2413 = { id = 0x51, family = 'opll' },
    ym3812 = { id = 0x5a, family = 'opl' },
    ym3526 = { id = 0x5b, family = 'opl' },
    ymf262 = { id = 0x5e, family = 'opl', banked = true },
    ay8910 = { id = 0xa0, family = 'ay' },
    ay8912 = { id = 0xa0, family = 'ay' },
    ay8913 = { id = 0xa0, family = 'ay' },
    ym2149 = { id = 0xa0, family = 'ay' },
}

local handlers = {
    ['ym2203_device::write'] = { chip = 'ym2203', ports = 2 },
    ['ym2413_device::write'] = { chip = 'ym2413', ports = 2 },
    ['ym3812_device::write'] = { chip = 'ym3812', ports = 2 },
    ['ym3526_device::write'] = { chip = 'ym3526', ports = 2 },
    ['ymf262_device::write'] = { chip = 'ymf262', ports = 4 },
}
for _, name in ipairs({ 'ay8910', 'ay8912', 'ay8913', 'ym2149' }) do
    handlers[name .. '_device::address_data_w'] = { chip = name, ports = 2 }
    handlers[name .. '_device::data_address_w'] = { chip = name, ports = 2, first_port = 1 }
end
for _, name in ipairs({ 'ym2203', 'ym2413', 'ym3812', 'ym3526', 'ay8910', 'ay8912', 'ay8913', 'ym2149' }) do
    handlers[name .. '_device::address_w'] = { chip = name, ports = 1 }
    handlers[name .. '_device::data_w'] = { chip = name, ports = 1, first_port = 1 }
end

-- MAME can omit the destination tags for direct delegates. Use the
-- source driver's verified sound CPU address map in that case.
local dual_ym2203_maps = {
    ['1943.cpp']     = { [0xe000] = ':ym1', [0xe002] = ':ym2' },
    ['blktiger.cpp'] = { [0xe000] = ':ym1', [0xe002] = ':ym2' },
    ['commando.cpp'] = { [0x8000] = ':ym1', [0x8002] = ':ym2' },
    ['gng.cpp']      = { [0xe000] = ':ym1', [0xe002] = ':ym2' },
    ['gunsmoke.cpp'] = { [0xe000] = ':ym1', [0xe002] = ':ym2' },
    ['lkage.cpp']    = { [0x9000] = ':ym1', [0xa000] = ':ym2' },
    ['lwings.cpp']   = { cpu = ':soundcpu', [0xe000] = ':2203a', [0xe002] = ':2203b' },
    ['sidearms.cpp'] = { [0xf000] = ':ym1', [0xf002] = ':ym2' },
    ['tigeroad.cpp'] = { [0x8000] = ':ym1', [0xa000] = ':ym2' },
}

local function find_mappings(machine)
    local chips, groups, mappings = {}, { opn = {}, ay = {}, opll = {}, opl = {} }, {}
    for _, dev in pairs(machine.devices) do
        local spec = chip_types[dev.shortname]
        if spec then
            local chip = { spec = spec, tag = dev.tag, name = dev.shortname, address = 0, page = 0 }
            chip.address_valid = spec.family ~= 'ay'
            if dev.shortname == 'ym2149' then
                local index = dev.items['0/m_flags']
                chip.ym2149_flags = index and emu.item(index):read(0)
            end
            chips[dev.tag] = chip
            table.insert(groups[spec.family], chip)
        end
    end

    local source = (machine.system.source_file or ''):match('[^/\\]+$')
    local dual_map = dual_ym2203_maps[source]
    for _, dev in pairs(machine.devices) do
        for name, space in pairs(dev.spaces) do
            if space.map and space.shift == 0 and (space.data_width == 8 or space.data_width == 16) then
                for _, entry in ipairs(space.map.entries) do
                    local handler = handlers[entry.write.name]
                    local stride = space.data_width >> 3
                    local start, finish = entry.address_start, entry.address_end
                    local lane = entry.mask ~= 0 and entry.mask or (1 << space.data_width) - 1
                    if handler and handler.ports == 1 and stride == 2 and start == finish
                        and (entry.address_mirror & 1) == 0 then
                        -- A byte at either address in the word uses one lane of the tap.
                        if entry.mask == 0 then
                            local low = ((start & 1) == 1) == (space.endianness == 'big')
                            lane = low and 0xff or 0xff00
                        end
                        start, finish = start & ~1, finish | 1
                    end
                    local shift = lane == 0xff and 0 or lane == 0xff00 and 8 or nil
                    if handler and shift and entry.write.bits == 8 and entry.address_mask == 0
                        and start % stride == 0 and finish - start == handler.ports * stride - 1 then
                        local spec = chip_types[handler.chip]
                        local group = groups[spec.family]
                        local chip = chips[entry.write.tag]
                        if not chip and #group == 1 then chip = group[1] end
                        if not chip and dual_map and handler.chip == 'ym2203' and handler.ports == 2
                            and dev.tag == (dual_map.cpu or ':audiocpu') and name == 'program'
                            and stride == 1 and entry.address_mirror == 0 then
                            chip = chips[dual_map[start]]
                        end
                        if chip and (chip.name == handler.chip or spec.family == 'ay' and chip.spec.family == 'ay') then
                            local first_port = handler.first_port or 0
                            chip.has_address = chip.has_address or handler.ports > 1 or first_port == 0
                            chip.has_data = chip.has_data or handler.ports > 1 or first_port == 1
                            mappings[#mappings + 1] = { chip = chip, space = space, lane = lane, shift = shift,
                                start = start, finish = finish, mirror = entry.address_mirror,
                                stride = stride, first_port = first_port }
                        end
                    end
                end
            end
        end
    end

    local routes, next_instance = {}, 0
    for _, family in ipairs({ 'opn', 'ay', 'opll', 'opl' }) do
        local group = groups[family]
        table.sort(group, function(a, b) return a.tag < b.tag end)
        local shares_opn = family == 'opn' or family == 'ay'
        local limit = shares_opn and 2 - next_instance or 1
        local assigned = 0
        for _, chip in ipairs(group) do
            if chip.name == 'ym2149' and chip.ym2149_flags == nil then
                emu.print_warning('ND7: cannot read YM2149 SEL for ' .. chip.tag .. '\n')
            elseif chip.has_address and chip.has_data and assigned < limit then
                chip.instance = shares_opn and next_instance or 0
                if shares_opn then next_instance = next_instance + 1 end
                routes[#routes + 1] = chip
                assigned = assigned + 1
            else
                emu.print_warning('ND7: no unambiguous direct map or free output for ' .. chip.tag .. '\n')
            end
        end
    end
    return routes, mappings
end

function exports.startplugin()
    local function send(data)
        if not active then return end
        if socket:write(data) ~= #data then
            active = false
            emu.print_error('ND7: connection lost or incomplete write; restart bridge and MAME\n')
        end
    end

    local function remove_taps()
        for _, tap in ipairs(taps) do tap:remove() end
        taps = {}
    end

    subscriptions.reset = emu.add_machine_reset_notifier(function()
        remove_taps()
        local routes, mappings = find_mappings(manager.machine)
        if #routes == 0 then return end

        local game = emu.romname()
        if #game > 255 then
            emu.print_error('ND7: machine name too long\n')
            return
        end
        local init = { string.pack('<I4s1', emu.pid(), game) }
        for _, chip in ipairs(routes) do
            if #chip.tag > 255 then
                emu.print_error('ND7: device tag too long\n')
                return
            end
            -- MAME saves YM2149_PIN26_LOW as 0x10. Bit 7 in the init instance byte
            -- tells the bridge to halve the -listxml clock before requesting the SSG clock.
            local instance = chip.instance
            if chip.ym2149_flags and (chip.ym2149_flags & 0x10) ~= 0 then instance = instance | 0x80 end
            init[#init + 1] = string.pack('BBs1', chip.spec.id, instance, chip.tag)
        end
        if not socket then
            socket = emu.file('', 3)
            local err = socket:open('socket.127.0.0.1:32151')
            if err then
                emu.print_error('ND7: cannot connect to bridge; start bridge before MAME: ' .. tostring(err) .. '\n')
                socket = nil
                return
            end
            active = true
        end

        send(string.pack('<c1s2', 'J', table.concat(init)) .. 'R')
        if paused then send('P') end
        for _, map in ipairs(mappings) do
            local chip = map.chip
            if chip.instance ~= nil then
                local function write_register(value)
                    send(string.char(0x56, chip.spec.id + chip.page, chip.instance, chip.address, value))
                end
                taps[#taps + 1] = map.space:install_write_tap(map.start, map.finish | map.mirror, 'nd7_sound', function(offset, data, mask)
                    local canonical = offset & (~map.mirror)
                    if canonical < map.start or canonical > map.finish or (mask & map.lane) == 0 then return end
                    local port = ((canonical - map.start) // map.stride) ~ map.first_port
                    local value = (data >> map.shift) & 0xff
                    if (port & 1) == 0 then
                        chip.address = value
                        if chip.spec.banked then
                            -- OPL3 data ports use the same latch. In OPL2 mode, only 0x105
                            -- is decoded in the upper bank.
                            chip.page = (chip.opl3 or value == 0x05) and (port >> 1) or 0
                        elseif chip.spec.family == 'ay' then
                            chip.address_valid = value < 0x10
                        elseif chip.spec.family == 'opll' then
                            chip.address_valid = value < 0x40
                        elseif value >= 0x2d and value <= 0x2f and chip.spec.family == 'opn' then
                            -- The YM2203 selects the prescaler on the address port.
                            write_register(0)
                        end
                    elseif chip.address_valid then
                        if chip.page == 1 and chip.address == 0x05 then chip.opl3 = (value & 1) ~= 0 end
                        write_register(value)
                    end
                end)
            end
        end
        for _, chip in ipairs(routes) do
            print(string.format('ND7: %s %s -> instance %d', chip.name, chip.tag, chip.instance))
        end
    end)
    subscriptions.pause = emu.add_machine_pause_notifier(function() paused = true; send('P') end)
    subscriptions.resume = emu.add_machine_resume_notifier(function() paused = false; send('U') end)
    subscriptions.stop = emu.add_machine_stop_notifier(function()
        send('Q')
        remove_taps()
        if socket then socket:close(); socket = nil end
        active = false
        paused = false
    end)
end
return exports
