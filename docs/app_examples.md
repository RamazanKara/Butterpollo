# App Examples
Since not all applications behave the same, we decided to create some examples to help you get started adding games
and applications to Sunshine.

> [!TIP]
> Throughout these examples, any fields not shown are left blank. You can enhance your experience by
> adding an image or a log file (via the `Output` field).

> [!WARNING]
> When a working directory is not specified, it defaults to the folder where the target application resides.


## Common Examples

### Desktop

| Field            | Value                      |
|------------------|----------------------------|
| Application Name | @code{}Desktop@endcode     |
| Image            | @code{}desktop.png@endcode |

### Steam Big Picture

> [!NOTE]
> Steam is launched as a detached command because Steam starts with a process that self updates itself and the original
> process is killed.

@tabs{
  @tab{Windows | <!-- -->
    \| Field                        \| Value                                     \|
    \|------------------------------\|-------------------------------------------\|
    \| Application Name             \| @code{}Steam Big Picture@endcode          \|
    \| Command Preporations -> Undo \| @code{}steam://close/bigpicture@endcode   \|
    \| Detached Commands            \| @code{}steam://open/bigpicture@endcode    \|
    \| Image                        \| @code{}steam.png@endcode                  \|
  }
}

### Epic Game Store game

> [!NOTE]
> Using the URI method will be the most consistent between various games.

#### URI

@tabs{
  @tab{Windows | <!-- -->
    \| Field            \| Value                                                                                                                                                 \|
    \|------------------\|-------------------------------------------------------------------------------------------------------------------------------------------------------\|
    \| Application Name \| @code{}Surviving Mars@endcode                                                                                                                         \|
    \| Commands         \| @code{}com.epicgames.launcher://apps/d759128018124dcabb1fbee9bb28e178%3A20729b9176c241f0b617c5723e70ec2d%3AOvenbird?action=launch&silent=true@endcode \|
  }
}

#### Binary (w/ working directory
@tabs{
  @tab{Windows | <!-- -->
    \| Field             \| Value                                                      \|
    \|-------------------\|------------------------------------------------------------\|
    \| Application Name  \| @code{}Surviving Mars@endcode                              \|
    \| Command           \| @code{}MarsEpic.exe@endcode                                \|
    \| Working Directory \| @code{}"C:\Program Files\Epic Games\SurvivingMars"@endcode \|
  }
}

#### Binary (w/o working directory)
@tabs{
  @tab{Windows | <!-- -->
    \| Field             \| Value                                                                   \|
    \|-------------------\|-------------------------------------------------------------------------\|
    \| Application Name  \| @code{}Surviving Mars@endcode                                           \|
    \| Command           \| @code{}"C:\Program Files\Epic Games\SurvivingMars\MarsEpic.exe"@endcode \|
  }
}

### Steam game

> [!NOTE]
> Using the URI method will be the most consistent between various games.

#### URI

@tabs{
  @tab{Windows | <!-- -->
    \| Field             \| Value                                   \|
    \|-------------------\|-----------------------------------------\|
    \| Application Name  \| @code{}Surviving Mars@endcode           \|
    \| Detached Commands \| @code{}steam://rungameid/464920@endcode \|
  }
}

#### Binary (w/ working directory
@tabs{
  @tab{Windows | <!-- -->
    \| Field             \| Value                                                                         \|
    \|-------------------\|-------------------------------------------------------------------------------\|
    \| Application Name  \| @code{}Surviving Mars@endcode                                                 \|
    \| Command           \| @code{}MarsSteam.exe@endcode                                                  \|
    \| Working Directory \| @code{}"C:\Program Files (x86)\Steam\steamapps\common\Surviving Mars"@endcode \|
  }
}

#### Binary (w/o working directory)
@tabs{
  @tab{Windows | <!-- -->
    \| Field             \| Value                                                                                       \|
    \|-------------------\|---------------------------------------------------------------------------------------------\|
    \| Application Name  \| @code{}Surviving Mars@endcode                                                               \|
    \| Command           \| @code{}"C:\Program Files (x86)\Steam\steamapps\common\Surviving Mars\MarsSteam.exe"@endcode \|
  }
}

### Prep Commands

#### Changing Resolution and Refresh Rate

##### Windows
Sunshine has built-in support for changing the resolution and refresh rate on Windows. If you prefer to use a
third-party tool, you can use *QRes* as an example.

###### QRes

> [!NOTE]
> This example uses the *QRes* tool to change the resolution and refresh rate.
> This tool can be downloaded from their [SourceForge repository](https://sourceforge.net/projects/qres).

| Prep Step | Command                                                                                                                   |
|-----------|---------------------------------------------------------------------------------------------------------------------------|
| Do        | @code{}cmd /C "FullPath\qres.exe /x:%SUNSHINE_CLIENT_WIDTH% /y:%SUNSHINE_CLIENT_HEIGHT% /r:%SUNSHINE_CLIENT_FPS%"@endcode |
| Undo      | @code{}FullPath\qres.exe /x:3840 /y:2160 /r:120@endcode                                                                   |

### Additional Considerations

#### Windows
**Elevating Commands (Windows)**

If you've installed Sunshine as a service (default), you can specify if a command should be elevated with
administrative privileges. Simply enable the elevated option in the WEB UI, or add it to the JSON configuration.
This is an option for both prep-cmd and regular commands and will launch the process with the current user without a
UAC prompt.

**Example**
```json
{
  "name": "Game With AntiCheat that Requires Admin",
  "output": "",
  "cmd": "ping 127.0.0.1",
  "exclude-global-prep-cmd": false,
  "elevated": true,
  "prep-cmd": [
    {
      "do": "powershell.exe -command \"Start-Streaming\"",
      "undo": "powershell.exe -command \"Stop-Streaming\"",
      "elevated": false
    }
  ],
  "image-path": ""
}
```

<div class="section_buttons">

| Previous                          |                                    Next |
|:----------------------------------|----------------------------------------:|
| [Configuration](configuration.md) | [Awesome-Sunshine](awesome_sunshine.md) |

</div>

<details style="display: none;">
  <summary></summary>
  [TOC]
</details>
